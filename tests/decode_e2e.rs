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

// Second, independent black-box reference decodes of the same three
// 9-7 codestreams (a different opaque CLI decoder than the one that
// produced the `-ref.pgm` files). The two references disagree with
// *each other* by ±1 at a handful of pixels whose reconstructed
// continuous value sits within ~0.004 of a half-integer — the
// inter-decoder rounding latitude ISO/IEC 15444-4 exists to budget
// (its Table C.1 allows peak errors up to 109 and MSE up to 743 on
// 9-7 test codestreams; §B.2.4 defines the MSE / peak metrics). This
// crate's decode is byte-exact against the second reference on all
// three fixtures (and across a 60-case black-box sweep), so the tests
// pin exactness against ref2 and the documented ±1 latitude against
// ref1.
const GRAY_97_REF2_PGM: &[u8] = include_bytes!("data/gray-32x32-97-ref2.pgm");
const GRAY_97_FULL_REF2_PGM: &[u8] = include_bytes!("data/gray-32x32-97full-ref2.pgm");

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

// §B.12.1.5 CPRL with **non-power-of-two** sub-sampling (XRsiz = YRsiz =
// 3). §B.12.1.3 (RPCL) and §B.12.1.4 (PCRL) require power-of-two
// XRsiz / YRsiz, but §B.12.1.5 (CPRL) states no such restriction: the
// component-major sweep emits each component's precincts in its own
// (y, x, resolution) order, so an arbitrary integer sub-sampling only
// rescales that one component's reference-grid corners. Three-component
// lossless 5-3, MCT off, NL = 2, one precinct per level. Pinned against
// a committed black-box reference decode (comment-scrubbed P6 PPM).
const RGB_CPRL_SUB3_53: &[u8] = include_bytes!("data/rgb-24x24-cprl-sub3-53.j2k");
const RGB_CPRL_SUB3_REF_PPM: &[u8] = include_bytes!("data/rgb-24x24-cprl-sub3-53-ref.ppm");

// CPRL, non-power-of-two sub-sampling (XRsiz = YRsiz = 3), **multiple
// precincts** (user-defined 16×16 precinct cells, 8×8 code-blocks,
// NL = 2) over a 48×48 three-component raster. Here the §B.12.1.5
// component-major sweep must order several precincts per resolution by
// their reference-grid corners, and the `ref_grid_*` projection scales
// each corner by the non-power-of-two XRsiz / YRsiz — so any slip in the
// non-pow2 corner arithmetic would mis-order the packet walk and corrupt
// the reconstruction. Pinned against a comment-scrubbed P6 reference.
const RGB_CPRL_SUB3_MP_53: &[u8] = include_bytes!("data/rgb-48x48-cprl-sub3-mp-53.j2k");
const RGB_CPRL_SUB3_MP_REF_PPM: &[u8] = include_bytes!("data/rgb-48x48-cprl-sub3-mp-53-ref.ppm");

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

// §D.6 selective-arithmetic-coding-bypass (Table A.19 bit 0) fixture:
// 40×40 gray, lossless 5-3, NL = 3, 8×8 code-blocks, single layer. The
// COD code-block-style byte sets bit 0, so the significance-propagation
// and magnitude-refinement passes from bit-plane 5 onward read raw
// (lazy) bits from a bit-stuffed §D.6 stream while every cleanup pass
// stays arithmetic-coded. The code-block contribution carves into the
// §B.10.7.2 / Table D.9 AC + raw codeword segments, so the packet
// header signals |T| lengths and the tier-1 driver alternates a fresh
// MqDecoder (AC spans) and RawBitReader (raw spans) on one continuous
// §D.3 schedule. A driver that decoded every pass through the MQ engine
// would desync at the first raw boundary, so pixel-exactness here pins
// the whole §D.6 path. COM markers scrubbed; encoded with an opaque CLI
// codec as a black box.
const GRAY_BYPASS_53: &[u8] = include_bytes!("data/gray-40x40-bypass-53.j2k");

// §D.6 bypass on the **9-7 irreversible** (lossy) path: 40×40 gray, NL
// = 3, 8×8 code-blocks, single layer, COD code-block-style bit 0 set.
// The raw (lazy) SP / MR passes from bit-plane 5 onward feed the
// scalar-quantised inverse 9-7 DWT, so this exercises the bypass
// dispatch through the irreversible reconstruction (not just the
// lossless 5-3 round-trip). Pinned against a committed black-box decode
// of the same codestream within the Annex F ±1 floating-point latitude.
// COM markers scrubbed.
const GRAY_BYPASS_97: &[u8] = include_bytes!("data/gray-40x40-bypass-97.j2k");
const GRAY_BYPASS_97_REF_PGM: &[u8] = include_bytes!("data/gray-40x40-bypass-97-ref.pgm");
const GRAY_BYPASS_97_REF2_PGM: &[u8] = include_bytes!("data/gray-40x40-bypass-97-ref2.pgm");

// §D.6 bypass across a 20×20 tile grid (2×2 = 4 tiles), lossless 5-3,
// NL = 3, 8×8 code-blocks, bypass style bit set. Each tile's
// code-blocks run their own §D.3 schedule, so the absolute pass cursor
// driving the Table D.9 segment split resets per tile / precinct: a
// leak of the cursor across tiles would mis-split the AC + raw segments
// and corrupt at least one tile. COM markers scrubbed.
const GRAY_BYPASS_TILED_53: &[u8] = include_bytes!("data/gray-40x40-bypass-tiled-53.j2k");

// §A.6.2 mixed-kernel-per-component pair: the *same* 16×16 gray raster
// encoded two ways with MCT off (`Rmct = 0`), NL = 2, 32×32 code-blocks
// (one block per sub-band), one precinct, one layer — once with the 5-3
// reversible filter and once with the 9-7 irreversible filter. The
// clean-room assembler below splices these two single-component streams
// into one two-component codestream whose COD default kernel is 5-3 and
// whose component-1 COC (Table A.15 SPcoc transformation = 0) selects
// the 9-7 kernel. Table A.17 permits a per-component kernel only when no
// multiple-component transform is signalled (`Rmct = 0`); with MCT off
// §G.1.2 reduces to a per-component DC level-shift + clamp, so the two
// kernels reconstruct independently and are re-interleaved into
// component order. COM markers are scrubbed by the assembler.
const GRAY_MK_53: &[u8] = include_bytes!("data/gray-16x16-mct0-53.j2k");
const GRAY_MK_97: &[u8] = include_bytes!("data/gray-16x16-mct0-97.j2k");

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

/// §D.6 selective-arithmetic-coding bypass (Table A.19 bit 0), the main
/// path: a 40×40 lossless 5-3 stream whose `COD` sets the bypass style.
/// The SP / MR passes from bit-plane 5 onward are raw and the cleanup
/// passes stay AC, split into the §B.10.7.2 / Table D.9 codeword
/// segments — pixel-exactness pins the AC ↔ raw dispatch, the raw
/// bit-stuffing reader and the segment-span split.
#[test]
fn gray_53_selective_arithmetic_coding_bypass_is_pixel_exact() {
    let cs = parse_codestream(GRAY_BYPASS_53).expect("parse");
    assert!(
        cs.header
            .cod
            .code_block_style_flags()
            .selective_arithmetic_coding_bypass(),
        "fixture must set the selective-arithmetic-coding-bypass style bit"
    );
    // And it must NOT set the termination-on-each-pass bit (so the
    // Table D.9 default AC + raw split — not the all-terminated split —
    // is exercised).
    assert!(
        !cs.header
            .cod
            .code_block_style_flags()
            .termination_on_each_coding_pass(),
        "fixture must not set the termination-on-each-coding-pass bit"
    );
    let img = decode_j2k(GRAY_BYPASS_53).expect("decode selective arithmetic coding bypass");
    assert_eq!(img.width, 40);
    assert_eq!(img.height, 40);
    assert_eq!(img.components.len(), 1);
    assert_eq!(img.components[0].precision_bits, 8);
    assert!(!img.components[0].is_signed);
    assert_eq!(img.components[0].samples, gray_40x40_pattern());
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
fn gray_97_selective_arithmetic_coding_bypass_tracks_black_box_reference() {
    // §D.6 bypass through the 9-7 irreversible reconstruction. The raw
    // SP / MR passes feed the scalar-quantised inverse DWT; the decode
    // must be **byte-exact** against the second independent black-box
    // reference and within the documented ±1 inter-reference rounding
    // latitude against the first (the two references themselves differ
    // by 1 at one half-integer-boundary pixel of this fixture).
    let cs = parse_codestream(GRAY_BYPASS_97).expect("parse");
    assert!(
        cs.header
            .cod
            .code_block_style_flags()
            .selective_arithmetic_coding_bypass(),
        "fixture must set the bypass style bit"
    );
    let img = decode_j2k(GRAY_BYPASS_97).expect("decode 9-7 bypass");
    assert_eq!(img.components.len(), 1);
    let c = &img.components[0];
    assert_eq!((c.width, c.height), (40, 40));

    // Reference 2: byte-exact.
    let (rw2, rh2, payload2) = pgm_payload(GRAY_BYPASS_97_REF2_PGM);
    assert_eq!((rw2, rh2), (40, 40));
    let ours: Vec<u8> = c.samples.iter().map(|&s| s as u8).collect();
    assert_eq!(
        ours.as_slice(),
        payload2,
        "9-7 bypass decode must match the second reference byte-exactly"
    );

    // Reference 1: the ISO/IEC 15444-4-style peak / MSE verdict
    // (Equations B-5 / B-4) within the documented latitude.
    let (rw, rh, payload) = pgm_payload(GRAY_BYPASS_97_REF_PGM);
    assert_eq!((rw, rh), (40, 40));
    assert_eq!(payload.len(), c.samples.len());
    let mut max_diff = 0i32;
    let mut sq = 0u64;
    for (&o, &refv) in c.samples.iter().zip(payload.iter()) {
        let d = (o - refv as i32).abs();
        max_diff = max_diff.max(d);
        sq += (d as u64) * (d as u64);
    }
    let mse = sq as f64 / payload.len() as f64;
    assert!(
        max_diff <= 1,
        "9-7 bypass decode deviates from reference 1 by {max_diff} (> 1)"
    );
    assert!(
        mse <= 0.001,
        "9-7 bypass decode MSE vs reference 1 is {mse} (> 0.001)"
    );
}

#[test]
fn gray_53_bypass_multi_tile_is_pixel_exact() {
    // §D.6 bypass across a 2×2 tile grid — pins that the Table D.9
    // segment split's absolute pass cursor is per code-block (resetting
    // per tile / precinct), not leaked across tiles.
    let cs = parse_codestream(GRAY_BYPASS_TILED_53).expect("parse");
    assert!(cs.tile_parts.len() >= 4, "expected one tile-part per tile");
    assert!(
        cs.header
            .cod
            .code_block_style_flags()
            .selective_arithmetic_coding_bypass(),
        "fixture must set the bypass style bit"
    );
    let img = decode_j2k(GRAY_BYPASS_TILED_53).expect("decode tiled bypass");
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

/// Decode a 32×32 9-7 fixture and return `(peak, mse)` — the ISO/IEC
/// 15444-4 §B.2.4 comparison metrics (Equations B-5 / B-4) — against a
/// committed black-box reference decode.
fn gray_97_deviation(j2k: &[u8], ref_pgm: &[u8]) -> (i32, f64) {
    let img = decode_j2k(j2k).expect("decode");
    assert_eq!(img.components.len(), 1);
    let c = &img.components[0];
    assert_eq!((c.width, c.height), (32, 32));

    let (rw, rh, payload) = pgm_payload(ref_pgm);
    assert_eq!((rw, rh), (32, 32));
    assert_eq!(payload.len(), c.samples.len());

    let mut max_diff = 0i32;
    let mut sq = 0u64;
    for (&ours, &refv) in c.samples.iter().zip(payload.iter()) {
        let d = (ours - refv as i32).abs();
        max_diff = max_diff.max(d);
        sq += (d as u64) * (d as u64);
    }
    (max_diff, sq as f64 / payload.len() as f64)
}

#[test]
fn gray_97_irreversible_full_quality_matches_black_box_references() {
    // 9-7 irreversible, scalar-expounded quantisation, 6 resolution
    // levels, every coding pass present (no rate truncation, so
    // Nb = Mb for every code-block — no §E.1.1.2 midpoint lift is in
    // play; the residual is pure inverse-DWT arithmetic).
    //
    // Byte-exact against the second independent reference; ±1 (at the
    // five pixels where the two references themselves disagree —
    // reconstructed values within 0.004 of a half-integer) against the
    // first, with the §B.2.4 MSE far inside the Table C.1 class
    // allowances.
    let (peak2, mse2) = gray_97_deviation(GRAY_97_FULL, GRAY_97_FULL_REF2_PGM);
    assert_eq!(
        (peak2, mse2),
        (0, 0.0),
        "full-quality 9-7 decode must match the second reference byte-exactly"
    );
    let (peak, mse) = gray_97_deviation(GRAY_97_FULL, GRAY_97_FULL_REF_PGM);
    assert!(
        peak <= 1,
        "full-quality 9-7 decode deviates from reference 1 by {peak} (> 1)"
    );
    assert!(
        mse <= 0.005,
        "full-quality 9-7 MSE vs reference 1 is {mse} (> 0.005)"
    );
}

#[test]
fn gray_97_irreversible_truncated_matches_black_box_references() {
    // Same source rate-limited 4:1 — coding passes are truncated
    // mid-bit-plane, so per the §E.1.1.2 NOTE Nb(u, v) differs across
    // one code-block: the coefficients the final partial pass reached
    // carry one more decoded magnitude bit than those it did not. The
    // tier-1 decoder tracks the §D.2.1 per-coefficient decoded-bit
    // count and the §E.1.1.2 reconstruction lifts each coefficient by
    // its own `r · 2^(Mb − Nb(u, v))` midpoint (round 302).
    //
    // The rate-truncated path is **byte-exact** against the second
    // independent reference (round 410; previously "±1 of reference"
    // was the best statement this suite could make). The residual ±1
    // against the first reference is the two references' own
    // disagreement — a single pixel whose reconstructed value lands
    // ~0.0008 above a half-integer — i.e. the inter-decoder rounding
    // latitude ISO/IEC 15444-4 budgets (Table C.1), not a defect in
    // the truncation handling.
    let (peak2, mse2) = gray_97_deviation(GRAY_97, GRAY_97_REF2_PGM);
    assert_eq!(
        (peak2, mse2),
        (0, 0.0),
        "truncated 9-7 decode must match the second reference byte-exactly"
    );
    let (peak, mse) = gray_97_deviation(GRAY_97, GRAY_97_REF_PGM);
    assert!(
        peak <= 1,
        "truncated 9-7 decode deviates from reference 1 by {peak} (> 1)"
    );
    assert!(
        mse <= 0.001,
        "truncated 9-7 MSE vs reference 1 is {mse} (> 0.001)"
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

/// Build a single-progression `POC` marker segment (T.800 §A.6.6,
/// Table A.32) covering the *whole* component / resolution / layer cube
/// of a `Csiz < 257` codestream, emitting it in `order`.
///
/// `RSpoc = 0`, `REpoc = nl + 1` (resolution levels are `0..=NL`, so the
/// exclusive end is `NL + 1`), `CSpoc = 0`, `CEpoc = csiz`,
/// `LYEpoc = layers`. Because the volume spans every packet, decoding
/// against it must yield exactly the same image the COD-default order
/// would — the only thing that changes is the §B.12.2 enumeration path
/// that is exercised.
fn make_poc_full_cube(order: ProgressionOrder, nl: u8, csiz: u8, layers: u16) -> Vec<u8> {
    use oxideav_jpeg2000::MARKER_POC;
    let ppoc = match order {
        ProgressionOrder::Lrcp => 0u8,
        ProgressionOrder::Rlcp => 1,
        ProgressionOrder::Rpcl => 2,
        ProgressionOrder::Pcrl => 3,
        ProgressionOrder::Cprl => 4,
        ProgressionOrder::Reserved(b) => b,
    };
    // Csiz < 257 → CSpoc / CEpoc are 8-bit; one entry is 7 bytes.
    // Lpoc = 2 (length) + 7 (one progression).
    let mut poc = Vec::new();
    poc.extend_from_slice(&MARKER_POC.to_be_bytes());
    poc.extend_from_slice(&9u16.to_be_bytes()); // Lpoc
    poc.push(0u8); // RSpoc
    poc.push(0u8); // CSpoc
    poc.extend_from_slice(&layers.to_be_bytes()); // LYEpoc
    poc.push(nl + 1); // REpoc (exclusive)
    poc.push(csiz); // CEpoc (exclusive)
    poc.push(ppoc); // Ppoc
    poc
}

/// Splice a complete marker segment into the main header immediately
/// before the first `SOT`.
fn inject_into_main_header(stream: &[u8], seg: &[u8]) -> Vec<u8> {
    let cs = parse_codestream(stream).expect("parse for main-header injection");
    let insert_at = cs.header.bytes_consumed;
    assert_eq!(
        u16::from_be_bytes([stream[insert_at], stream[insert_at + 1]]),
        0xFF90, // SOT
        "insertion point must be the first SOT"
    );
    let mut out = Vec::with_capacity(stream.len() + seg.len());
    out.extend_from_slice(&stream[..insert_at]);
    out.extend_from_slice(seg);
    out.extend_from_slice(&stream[insert_at..]);
    out
}

/// §A.6.6 main-header `POC` restating the gray 5-3 fixture's COD-default
/// LRCP order over the whole cube. The POC now *drives* the §B.12.2
/// packet enumeration (instead of the COD's `SGcod` order), so a
/// pixel-exact decode proves the POC path is wired and produces the same
/// packet visitation as the default LRCP walk.
#[test]
fn gray_53_main_header_poc_lrcp_is_pixel_exact() {
    let poc = make_poc_full_cube(ProgressionOrder::Lrcp, 2, 1, 1);
    let injected = inject_into_main_header(GRAY_53, &poc);
    // The POC must now parse rather than be rejected as NotImplemented.
    parse_codestream(&injected).expect("parse with main-header POC");
    let img = decode_j2k(&injected).expect("decode with main-header POC");
    assert_eq!(img.components.len(), 1);
    assert_eq!(img.components[0].samples, gray_17x13_pattern());
}

/// §A.6.6 main-header `POC` restating LRCP over the multi-layer gray
/// fixture (5 quality layers). The POC volume's `LYEpoc = layers` spans
/// every quality layer, so the §B.12.2 per-(component, resolution,
/// precinct) "next unsent layer" cursor must walk all five layers in
/// LRCP order — matching the physical packet layout. Pixel-exactness
/// pins the multi-layer POC-volume cursor.
///
/// (Restating a *different* order, e.g. RLCP, would not be pixel-exact:
/// the codestream's packet bodies are physically laid out in the
/// encoded LRCP order, and tier-2 reads them sequentially in the
/// enumerated order — a POC that contradicts the physical layout
/// describes a different, malformed stream, so it is intentionally not
/// asserted here.)
#[test]
fn gray_53_multilayer_main_header_poc_lrcp_is_pixel_exact() {
    let cs = parse_codestream(GRAY_MULTILAYER_53).expect("parse");
    let layers = cs.header.cod.layers;
    assert!(layers >= 2, "fixture must be multi-layer");
    assert_eq!(cs.header.cod.progression, ProgressionOrder::Lrcp);
    let poc = make_poc_full_cube(ProgressionOrder::Lrcp, 2, 1, layers);
    let injected = inject_into_main_header(GRAY_MULTILAYER_53, &poc);
    let img = decode_j2k(&injected).expect("decode multi-layer with LRCP POC");
    assert_eq!((img.width, img.height), (64, 64));
    assert_eq!(img.components[0].samples, gray_64x64_pattern());
}

/// §A.6.6 main-header `POC` over the 3-component RGB/RCT fixture,
/// restating LRCP over the full `CSpoc = 0 .. CEpoc = 3` component
/// sub-range. Exercises the per-component sub-range iteration of the POC
/// volume; the inverse RCT and all three planes must come back exact.
#[test]
fn rgb_rct_53_main_header_poc_lrcp_is_pixel_exact() {
    let cs = parse_codestream(RGB_RCT_53).expect("parse");
    assert_eq!(cs.header.siz.components.len(), 3);
    let poc = make_poc_full_cube(ProgressionOrder::Lrcp, 2, 3, 1);
    let injected = inject_into_main_header(RGB_RCT_53, &poc);
    let img = decode_j2k(&injected).expect("decode 3-comp with main-header POC");
    assert_eq!(img.components.len(), 3);
    let expected = rgb_16x16_pattern();
    for (c, exp) in img.components.iter().zip(expected.iter()) {
        assert_eq!(&c.samples, exp);
    }
}

/// §A.6.6 main-header `POC` restating the **position-keyed** RPCL order
/// over the 48×32 RGB fixture whose COD default is already RPCL. This
/// drives the POC enumerator down the `ComponentPositionInfo` path
/// (resolution-level / precinct-position keyed) rather than the
/// layer-keyed one, with the §B.12.1.3 power-of-two XRsiz/YRsiz check.
#[test]
fn rgb_rpcl_53_main_header_poc_rpcl_is_pixel_exact() {
    let cs = parse_codestream(RGB_RPCL_53).expect("parse");
    assert_eq!(cs.header.cod.progression, ProgressionOrder::Rpcl);
    let poc = make_poc_full_cube(ProgressionOrder::Rpcl, 2, 3, 1);
    let injected = inject_into_main_header(RGB_RPCL_53, &poc);
    let img = decode_j2k(&injected).expect("decode with RPCL POC");
    assert_eq!((img.width, img.height), (48, 32));
    let expected = rgb_48x32_pattern();
    for (c, exp) in img.components.iter().zip(expected.iter()) {
        assert_eq!(&c.samples, exp);
    }
}

/// §A.6.6 precedence path: a **tile-part** `POC` (in the first tile-part
/// header) restating LRCP over the gray fixture's cube. Proves the
/// `Tile-part POC` resolution route decodes (not just the main-header
/// one), reusing the Psot-growing tile-part injector.
#[test]
fn gray_53_tile_part_poc_lrcp_is_pixel_exact() {
    let poc = make_poc_full_cube(ProgressionOrder::Lrcp, 2, 1, 1);
    let injected = inject_into_first_tile_part_header(GRAY_53, &poc);
    let img = decode_j2k(&injected).expect("decode with tile-part POC");
    assert_eq!(img.components.len(), 1);
    assert_eq!(img.components[0].samples, gray_17x13_pattern());
}

/// Set Table A.19 bit 4 (predictable termination) in the main-header
/// `COD`'s code-block-style byte. The `COD` layout (Table A.15) is
/// `marker(2) Lcod(2) Scod(1) SGcod(prog 1, layers 2, MCT 1) SPcod(NL 1,
/// xcb 1, ycb 1, style 1, …)`, so the style byte sits 13 bytes past the
/// marker code.
fn set_predictable_termination(stream: &[u8]) -> Vec<u8> {
    use oxideav_jpeg2000::MARKER_COD;
    let cs = parse_codestream(stream).expect("parse");
    let end = cs.header.bytes_consumed;
    let mut out = stream.to_vec();
    let mut pos = 2usize;
    while pos + 4 <= end {
        let marker = u16::from_be_bytes([out[pos], out[pos + 1]]);
        let len = u16::from_be_bytes([out[pos + 2], out[pos + 3]]) as usize;
        if marker == MARKER_COD {
            let style_off = pos + 2 + 2 + 1 + 4 + 3;
            out[style_off] |= 0x10; // Table A.19 bit 4
            return out;
        }
        pos += 2 + len;
    }
    panic!("no COD in main header");
}

/// §D.4.2 predictable termination (Table A.19 bit 4) constrains the
/// *encoder's* flush procedure only — the decode path is identical
/// either way, and the §D.4.1 synthesised 0xFF extension applies as
/// usual ("Often at that point there are more symbols to be decoded.
/// Therefore, the decoder shall extend the input bit stream … with
/// 0xFF bytes"). Flipping the signalling bit on a stream encoded
/// without it must therefore leave the decode pixel-exact: there is
/// no decoder-side landing-position contract to violate (§J.7 names
/// the §D.5 segmentation symbol as the in-stream error-detection
/// mechanism, not bit 4). Through round 409 the decoder enforced an
/// invented exact-landing check here, which mis-rejected real
/// predictable-termination codestreams (their final renormalisations
/// routinely read into the synthesised fill).
#[test]
fn gray_53_predictable_termination_bit_does_not_change_decode() {
    // Sanity: the unmodified fixture decodes pixel-exact.
    assert_eq!(
        decode_j2k(GRAY_53).expect("baseline").components[0].samples,
        gray_17x13_pattern()
    );
    let mutated = set_predictable_termination(GRAY_53);
    let img = decode_j2k(&mutated).expect("decode with bit 4 set");
    assert_eq!(
        img.components[0].samples,
        gray_17x13_pattern(),
        "Table A.19 bit 4 must not change the decoded samples"
    );
}

// §D.4.2 predictable-termination fixtures from a real (black-box CLI)
// encoder: the same 40×40 gray raster, lossless 5-3, NL = 3, 8×8
// code-blocks, encoded with Table A.19 style combinations that all
// include bit 4 (0x10, "predictable termination"):
//
//   * mode16 — bit 4 alone (single codeword segment per block).
//   * mode17 — bits 0 + 4 (selective AC bypass, §D.6: the raw MR
//     segments' padding uses the §D.6 alternating 0/1 sequence).
//   * mode20 — bits 2 + 4 (termination on each coding pass, §B.10.7.2
//     per-pass segments, each flushed by the §D.4.2 procedure).
//   * mode48 — bits 4 + 5 (segmentation symbols, §D.5 — the NOTE's
//     "with or without the predictable termination" composition).
//   * mode63 — bits 0–5 all set (bypass + reset + termall + vertically
//     causal + predictable + segmentation symbols).
//
// Decoding a §D.4.2-flushed segment routinely finishes its final
// renormalisations inside the §D.4.1 synthesised 0xFF fill, so these
// pin that no landing-position check mis-fires; the reversible path
// must stay pixel-exact. COM markers scrubbed.
const GRAY_MODE16_53: &[u8] = include_bytes!("data/gray-40x40-mode16-53.j2k");
const GRAY_MODE17_53: &[u8] = include_bytes!("data/gray-40x40-mode17-53.j2k");
const GRAY_MODE20_53: &[u8] = include_bytes!("data/gray-40x40-mode20-53.j2k");
const GRAY_MODE48_53: &[u8] = include_bytes!("data/gray-40x40-mode48-53.j2k");
const GRAY_MODE63_53: &[u8] = include_bytes!("data/gray-40x40-mode63-53.j2k");

/// Shared body for the §D.4.2 predictable-termination fixtures:
/// assert the COD carries the expected Table A.19 style byte, then
/// assert the reversible 5-3 decode reproduces the source raster.
fn assert_mode_fixture_pixel_exact(j2k: &[u8], expected_style: u8) {
    let cs = parse_codestream(j2k).expect("parse");
    assert_eq!(
        cs.header.cod.code_block_style, expected_style,
        "fixture COD code-block style byte"
    );
    assert!(
        cs.header
            .cod
            .code_block_style_flags()
            .predictable_termination(),
        "fixture must signal predictable termination (bit 4)"
    );
    let img = decode_j2k(j2k).expect("decode predictable-termination fixture");
    assert_eq!((img.width, img.height), (40, 40));
    assert_eq!(img.components.len(), 1);
    assert_eq!(img.components[0].samples, gray_40x40_pattern());
}

#[test]
fn gray_53_predictable_termination_alone_is_pixel_exact() {
    assert_mode_fixture_pixel_exact(GRAY_MODE16_53, 0x10);
}

#[test]
fn gray_53_predictable_termination_with_bypass_is_pixel_exact() {
    assert_mode_fixture_pixel_exact(GRAY_MODE17_53, 0x11);
}

#[test]
fn gray_53_predictable_termination_with_termall_is_pixel_exact() {
    assert_mode_fixture_pixel_exact(GRAY_MODE20_53, 0x14);
}

#[test]
fn gray_53_predictable_termination_with_segmentation_symbols_is_pixel_exact() {
    assert_mode_fixture_pixel_exact(GRAY_MODE48_53, 0x30);
}

#[test]
fn gray_53_all_six_style_bits_is_pixel_exact() {
    assert_mode_fixture_pixel_exact(GRAY_MODE63_53, 0x3F);
}

#[test]
fn truncated_codestream_is_rejected() {
    let cut = &GRAY_53[..GRAY_53.len() / 2];
    assert!(decode_j2k(cut).is_err());
}

/// §B.6 / Table A.21: a user-defined precinct exponent byte "may only
/// equal zero at the resolution level corresponding to the NLLL band"
/// (`r = 0`). Rewrite one `r > 0` precinct byte of the multi-precinct
/// fixture's `COD` to `0x00` and assert the stream is rejected with
/// `Error::InvalidPrecinctSize` — decoding on would build a precinct
/// lattice no conforming encoder can have used and desynchronise the
/// packet walk. (Independent black-box reference decoders likewise
/// refuse such a stream at the `COD` marker.)
#[test]
fn gray_53_zero_precinct_exponent_above_r0_is_rejected() {
    use oxideav_jpeg2000::{Error, MARKER_COD};

    let cs = parse_codestream(GRAY_MULTIPRECINCT_53).expect("parse");
    assert!(
        cs.header.cod.user_defined_precincts,
        "fixture must carry user-defined precincts"
    );
    let nl = cs.header.cod.decomposition_levels as usize;
    assert_eq!(
        cs.header.cod.precincts.len(),
        nl + 1,
        "Table A.21: NL + 1 precinct bytes"
    );

    // Locate the COD in the main header; the precinct bytes trail the
    // fixed SPcod fields (Table A.12: marker(2) Lcod(2) Scod(1)
    // SGcod(4) SPcod fixed(5), then NL + 1 precinct bytes).
    let mut out = GRAY_MULTIPRECINCT_53.to_vec();
    let m = MARKER_COD.to_be_bytes();
    let cod_at = (2..cs.header.bytes_consumed)
        .find(|&i| out[i] == m[0] && out[i + 1] == m[1])
        .expect("COD marker present");
    let precinct0_at = cod_at + 2 + 2 + 1 + 4 + 5;

    // Sanity: the located bytes match the parsed precinct vector.
    assert_eq!(
        &out[precinct0_at..precinct0_at + nl + 1],
        cs.header.cod.precincts.as_slice()
    );

    // Zero the r = 1 byte (the first above-NLLL resolution level).
    out[precinct0_at + 1] = 0x00;
    assert_eq!(
        decode_j2k(&out),
        Err(Error::InvalidPrecinctSize),
        "PPx = PPy = 0 at r = 1 must be rejected"
    );

    // The untouched fixture still decodes pixel-exact (guards against
    // the mutation helper drifting).
    let img = decode_j2k(GRAY_MULTIPRECINCT_53).expect("baseline decode");
    assert_eq!(img.components[0].samples, gray_40x40_pattern());
}

// -------------------------------------------------------------------------
// §A.6.2 mixed-kernel-per-component (Rmct = 0) assembly + decode.
// -------------------------------------------------------------------------

/// Find the byte offset of a two-byte marker in the main header (before
/// the first `SOT`). Panics if absent.
fn find_marker(bytes: &[u8], marker: u16) -> usize {
    let m = marker.to_be_bytes();
    let mut pos = 2usize; // skip SOC
    loop {
        assert!(pos + 4 <= bytes.len(), "marker {marker:#06x} not found");
        if bytes[pos] == m[0] && bytes[pos + 1] == m[1] {
            return pos;
        }
        // SOT ends the main header.
        if bytes[pos] == 0xFF && bytes[pos + 1] == 0x90 {
            panic!("marker {marker:#06x} not found before SOT");
        }
        let len = u16::from_be_bytes([bytes[pos + 2], bytes[pos + 3]]) as usize;
        pos += 2 + len;
    }
}

/// Read a `(marker, Lxxx, payload)` segment's payload slice.
fn marker_payload(bytes: &[u8], off: usize) -> &[u8] {
    let len = u16::from_be_bytes([bytes[off + 2], bytes[off + 3]]) as usize;
    &bytes[off + 4..off + 2 + len]
}

/// Extract the tile body (bytes after `SOD`, minus any trailing `EOC`).
fn tile_body(bytes: &[u8]) -> &[u8] {
    let sod = find_sod(bytes);
    let mut body = &bytes[sod + 2..];
    if body.len() >= 2 && body[body.len() - 2] == 0xFF && body[body.len() - 1] == 0xD9 {
        body = &body[..body.len() - 2];
    }
    body
}

fn find_sod(bytes: &[u8]) -> usize {
    let mut pos = 0usize;
    while pos + 2 <= bytes.len() {
        if bytes[pos] == 0xFF && bytes[pos + 1] == 0x93 {
            return pos;
        }
        pos += 1;
    }
    panic!("no SOD");
}

/// Clean-room assembler: splice two single-component single-tile J2K
/// codestreams — one 5-3, one 9-7, both `Rmct = 0`, identical geometry,
/// one layer / one precinct — into a single two-component codestream.
///
/// The COD default kernel stays 5-3 and the progression is rewritten to
/// **CPRL** (component-major) so component 0's packets precede all of
/// component 1's. A COC selects the 9-7 kernel for component 1 (Table
/// A.15 SPcoc transformation byte) and a QCC carries the 9-7 stream's
/// quantisation for it. The two bodies concatenate in CPRL order:
/// component 0's (r0, r1, r2) packets then component 1's — which for a
/// single-layer / single-precinct stream is exactly each source body's
/// own LRCP ordering, so no packet-level re-interleave is needed.
///
/// Markers are built from the T.800 Annex A layout; the packet bodies
/// are the opaque encoder's bytes, spliced verbatim.
fn assemble_mixed_kernel(base53: &[u8], other97: &[u8]) -> Vec<u8> {
    // -- SIZ: bump Csiz 1 -> 2 and append a duplicate component
    //    descriptor (both components share the 8-bit / 1×1 geometry). --
    let siz_off = find_marker(base53, 0xFF51);
    let siz = marker_payload(base53, siz_off);
    // SIZ layout: Rsiz(2) Xsiz(4) Ysiz(4) XOsiz(4) YOsiz(4) XTsiz(4)
    // YTsiz(4) XTOsiz(4) YTOsiz(4) Csiz(2) then Csiz×{Ssiz(1) XRsiz(1)
    // YRsiz(1)}.
    let csiz_pos = 2 + 4 * 8; // 38
    let csiz = u16::from_be_bytes([siz[csiz_pos], siz[csiz_pos + 1]]);
    assert_eq!(csiz, 1, "assembler expects single-component sources");
    let comp_desc = &siz[csiz_pos + 2..csiz_pos + 5]; // Ssiz XRsiz YRsiz
    let mut new_siz = Vec::new();
    new_siz.extend_from_slice(&siz[..csiz_pos]);
    new_siz.extend_from_slice(&2u16.to_be_bytes()); // Csiz = 2
    new_siz.extend_from_slice(comp_desc); // component 0
    new_siz.extend_from_slice(comp_desc); // component 1 (identical)
    let new_siz_seg = wrap_marker(0xFF51, &new_siz);

    // -- COD: keep 5-3 default kernel, rewrite progression -> CPRL. --
    let cod_off = find_marker(base53, 0xFF52);
    let cod = marker_payload(base53, cod_off);
    let mut new_cod = cod.to_vec();
    // COD payload: Scod(1) SGcod{prog(1) layers(2) mct(1)} SPcod{…}.
    new_cod[1] = 0x04; // SGcod progression = CPRL (Table A.16)
    assert_eq!(new_cod[4], 0x00, "COD MCT must be off");
    let cod_transform = *new_cod.last().unwrap();
    assert_eq!(cod_transform, 0x01, "base COD must be the 5-3 kernel");
    let new_cod_seg = wrap_marker(0xFF52, &new_cod);

    // -- COC for component 1: SPcoc copied from the 9-7 stream's COD. --
    let cod97_off = find_marker(other97, 0xFF52);
    let cod97 = marker_payload(other97, cod97_off);
    // SPcoc = COD SPcod tail (after Scod + SGcod). For a Csiz < 257
    // stream Ccoc is one byte.
    let spcod97 = &cod97[5..];
    assert_eq!(*spcod97.last().unwrap(), 0x00, "other COD must be 9-7");
    let mut coc_payload = vec![0x01u8, 0x00]; // Ccoc = 1, Scoc = 0
    coc_payload.extend_from_slice(spcod97);
    let coc_seg = wrap_marker(0xFF53, &coc_payload);

    // -- QCD: keep the 5-3 stream's (component 0 default). --
    let qcd_off = find_marker(base53, 0xFF5C);
    let qcd = marker_payload(base53, qcd_off);
    let new_qcd_seg = wrap_marker(0xFF5C, qcd);

    // -- QCC for component 1: the 9-7 stream's QCD payload. --
    let qcd97_off = find_marker(other97, 0xFF5C);
    let qcd97 = marker_payload(other97, qcd97_off);
    let mut qcc_payload = vec![0x01u8]; // Cqcc = 1
    qcc_payload.extend_from_slice(qcd97);
    let qcc_seg = wrap_marker(0xFF5D, &qcc_payload);

    // -- Tile body: component 0 (5-3) then component 1 (9-7). --
    let body53 = tile_body(base53);
    let body97 = tile_body(other97);
    let mut body = Vec::new();
    body.extend_from_slice(body53);
    body.extend_from_slice(body97);

    // -- SOT: Psot = length from SOT marker to the end of the tile data.
    //    Layout Lsot(2)=10 Isot(2) Psot(4) TPsot(1) TNsot(1). --
    let sot: Vec<u8> = {
        let mut s = Vec::new();
        s.extend_from_slice(&0xFF90u16.to_be_bytes());
        s.extend_from_slice(&10u16.to_be_bytes()); // Lsot
        s.extend_from_slice(&0u16.to_be_bytes()); // Isot = tile 0
                                                  // Psot patched below once the body length is known.
        s.extend_from_slice(&0u32.to_be_bytes());
        s.push(0); // TPsot
        s.push(1); // TNsot
        s
    };
    let sod = 0xFF93u16.to_be_bytes();

    // -- Assemble: SOC SIZ COD COC QCD QCC SOT SOD body EOC. --
    let mut out = Vec::new();
    out.extend_from_slice(&0xFF4Fu16.to_be_bytes()); // SOC
    out.extend_from_slice(&new_siz_seg);
    out.extend_from_slice(&new_cod_seg);
    out.extend_from_slice(&coc_seg);
    out.extend_from_slice(&new_qcd_seg);
    out.extend_from_slice(&qcc_seg);
    let sot_off = out.len();
    out.extend_from_slice(&sot);
    out.extend_from_slice(&sod);
    out.extend_from_slice(&body);
    out.extend_from_slice(&0xFFD9u16.to_be_bytes()); // EOC

    // Psot = bytes from the SOT marker through the end of the tile data
    // (excludes the trailing EOC). Psot field sits at sot_off + 6.
    let psot = (out.len() - 2 - sot_off) as u32;
    out[sot_off + 6..sot_off + 10].copy_from_slice(&psot.to_be_bytes());
    out
}

/// Like [`assemble_mixed_kernel`] but places the component-1 `COC` /
/// `QCC` overrides in the **tile-part header** (between `SOT` and `SOD`)
/// instead of the main header, exercising the §A.6.1 tile-part
/// precedence route for a mixed-kernel tile. The main header carries
/// only the two-component `SIZ`, the default-5-3 `COD` (CPRL) and the
/// 5-3 `QCD`; the tile-part header restates component 1 as 9-7.
fn assemble_mixed_kernel_tilepart(base53: &[u8], other97: &[u8]) -> Vec<u8> {
    let siz = marker_payload(base53, find_marker(base53, 0xFF51));
    let csiz_pos = 2 + 4 * 8;
    assert_eq!(u16::from_be_bytes([siz[csiz_pos], siz[csiz_pos + 1]]), 1);
    let comp_desc = &siz[csiz_pos + 2..csiz_pos + 5];
    let mut new_siz = Vec::new();
    new_siz.extend_from_slice(&siz[..csiz_pos]);
    new_siz.extend_from_slice(&2u16.to_be_bytes());
    new_siz.extend_from_slice(comp_desc);
    new_siz.extend_from_slice(comp_desc);

    let mut new_cod = marker_payload(base53, find_marker(base53, 0xFF52)).to_vec();
    new_cod[1] = 0x04; // CPRL
    assert_eq!(*new_cod.last().unwrap(), 0x01, "base must be 5-3");

    let cod97 = marker_payload(other97, find_marker(other97, 0xFF52));
    let spcod97 = &cod97[5..];
    let mut coc_payload = vec![0x01u8, 0x00];
    coc_payload.extend_from_slice(spcod97);
    let coc_seg = wrap_marker(0xFF53, &coc_payload);

    let qcd = marker_payload(base53, find_marker(base53, 0xFF5C));
    let qcd97 = marker_payload(other97, find_marker(other97, 0xFF5C));
    let mut qcc_payload = vec![0x01u8];
    qcc_payload.extend_from_slice(qcd97);
    let qcc_seg = wrap_marker(0xFF5D, &qcc_payload);

    let mut body = Vec::new();
    body.extend_from_slice(tile_body(base53));
    body.extend_from_slice(tile_body(other97));

    let mut out = Vec::new();
    out.extend_from_slice(&0xFF4Fu16.to_be_bytes()); // SOC
    out.extend_from_slice(&wrap_marker(0xFF51, &new_siz));
    out.extend_from_slice(&wrap_marker(0xFF52, &new_cod));
    out.extend_from_slice(&wrap_marker(0xFF5C, qcd));
    let sot_off = out.len();
    out.extend_from_slice(&0xFF90u16.to_be_bytes());
    out.extend_from_slice(&10u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.push(0); // TPsot = 0 (overrides allowed only here)
    out.push(1);
    // Tile-part header markers: COC + QCC for component 1, then SOD.
    out.extend_from_slice(&coc_seg);
    out.extend_from_slice(&qcc_seg);
    out.extend_from_slice(&0xFF93u16.to_be_bytes()); // SOD
    out.extend_from_slice(&body);
    out.extend_from_slice(&0xFFD9u16.to_be_bytes());
    let psot = (out.len() - 2 - sot_off) as u32;
    out[sot_off + 6..sot_off + 10].copy_from_slice(&psot.to_be_bytes());
    out
}

/// The §A.6.1 tile-part precedence route also decodes a mixed-kernel
/// tile: with the component-1 `COC` (9-7) and `QCC` living in the
/// tile-part header rather than the main header, each component must
/// still reconstruct identically to its single-component decode.
#[test]
fn mixed_kernel_via_tile_part_header_reconstructs_both_lanes() {
    let ref53 = decode_j2k(GRAY_MK_53).expect("decode 5-3 source");
    let ref97 = decode_j2k(GRAY_MK_97).expect("decode 9-7 source");
    let stream = assemble_mixed_kernel_tilepart(GRAY_MK_53, GRAY_MK_97);
    let img = decode_j2k(&stream).expect("decode tile-part mixed-kernel stream");
    assert_eq!(img.components.len(), 2);
    assert_eq!(img.components[0].samples, ref53.components[0].samples);
    assert_eq!(img.components[1].samples, ref97.components[0].samples);
    assert_ne!(img.components[0].samples, img.components[1].samples);
}

/// Wrap a payload in a `marker Lxxx payload` segment (Lxxx counts the
/// length field itself plus the payload).
fn wrap_marker(marker: u16, payload: &[u8]) -> Vec<u8> {
    let mut seg = Vec::with_capacity(4 + payload.len());
    seg.extend_from_slice(&marker.to_be_bytes());
    seg.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    seg.extend_from_slice(payload);
    seg
}

/// A two-component codestream whose components use *different* wavelet
/// kernels (component 0 = 5-3, component 1 = 9-7), MCT off. Each
/// component must reconstruct exactly as if it were decoded from its own
/// single-component stream — the mixed-kernel reassembly interleaves the
/// two lanes back into component order.
#[test]
fn mixed_kernel_per_component_mct0_reconstructs_both_lanes() {
    // Reference: decode each single-component source on its own.
    let ref53 = decode_j2k(GRAY_MK_53).expect("decode 5-3 source");
    let ref97 = decode_j2k(GRAY_MK_97).expect("decode 9-7 source");
    assert_eq!(ref53.components.len(), 1);
    assert_eq!(ref97.components.len(), 1);

    let mixed = assemble_mixed_kernel(GRAY_MK_53, GRAY_MK_97);
    let img = decode_j2k(&mixed).expect("decode mixed-kernel two-component stream");
    assert_eq!(img.components.len(), 2, "two components expected");
    assert_eq!(img.width, 16);
    assert_eq!(img.height, 16);

    // Component 0 rode the 5-3 lane; component 1 rode the 9-7 lane. Each
    // must match its own single-component decode bit-for-bit.
    assert_eq!(
        img.components[0].samples, ref53.components[0].samples,
        "component 0 (5-3 kernel) diverged from its single-component decode"
    );
    assert_eq!(
        img.components[1].samples, ref97.components[0].samples,
        "component 1 (9-7 kernel) diverged from its single-component decode"
    );

    // The two lanes carry genuinely different reconstructions (5-3 is
    // lossless, 9-7 is lossy over the same raster), so a lane mix-up
    // would have surfaced above; assert they differ to keep the test
    // honest about exercising the interleave.
    assert_ne!(
        img.components[0].samples, img.components[1].samples,
        "the 5-3 and 9-7 lanes should reconstruct differently"
    );
}

/// A mixed-kernel tile that *also* signals a multiple-component
/// transform (`Rmct = 1`) is rejected: Table A.17 pairs a single kernel
/// with the MCT, so feeding two different lanes into the RCT/ICT first
/// three inputs is undefined. The assembler flips the COD MCT byte on.
#[test]
fn mixed_kernel_with_mct_is_rejected() {
    let mut mixed = assemble_mixed_kernel(GRAY_MK_53, GRAY_MK_97);
    // Flip the COD SGcod MCT byte to 1. COD sits right after SIZ; locate
    // it and set the MCT byte (Scod(1) + SGcod{prog(1) layers(2) mct(1)}
    // => marker(2) Lcod(2) + 1 + 1 + 2 = offset 8 from the marker).
    let cod_off = find_marker(&mixed, 0xFF52);
    mixed[cod_off + 8] = 0x01; // Rmct = 1
    assert!(
        decode_j2k(&mixed).is_err(),
        "a mixed-kernel tile with an active MCT must be rejected"
    );
}

/// Generalised clean-room assembler: splice N single-component,
/// single-tile J2K streams — each of identical geometry, MCT off, one
/// layer / one precinct — into one N-component CPRL codestream. The COD
/// default kernel is component 0's; every component whose kernel differs
/// from the default gets a `COC` (Table A.15 SPcoc transformation) plus
/// a `QCC` copied from that source stream's `QCD`. Bodies concatenate in
/// CPRL component order. This exercises the mixed-kernel lane interleave
/// with **non-adjacent** lane assignments (e.g. 9-7 / 5-3 / 9-7), which a
/// two-component splice cannot.
fn assemble_multi_component(sources: &[&[u8]]) -> Vec<u8> {
    assert!(!sources.is_empty());
    let base = sources[0];
    let siz = marker_payload(base, find_marker(base, 0xFF51));
    let csiz_pos = 2 + 4 * 8;
    assert_eq!(u16::from_be_bytes([siz[csiz_pos], siz[csiz_pos + 1]]), 1);
    let comp_desc = &siz[csiz_pos + 2..csiz_pos + 5];

    // -- SIZ with Csiz = N, N identical component descriptors. --
    let mut new_siz = Vec::new();
    new_siz.extend_from_slice(&siz[..csiz_pos]);
    new_siz.extend_from_slice(&(sources.len() as u16).to_be_bytes());
    for _ in sources {
        new_siz.extend_from_slice(comp_desc);
    }

    // -- COD: component 0's kernel is the default; progression -> CPRL. --
    let cod0 = marker_payload(base, find_marker(base, 0xFF52)).to_vec();
    let default_transform = *cod0.last().unwrap();
    let mut new_cod = cod0.clone();
    new_cod[1] = 0x04; // CPRL
    assert_eq!(new_cod[4], 0x00, "COD MCT must be off");

    let mut out = Vec::new();
    out.extend_from_slice(&0xFF4Fu16.to_be_bytes()); // SOC
    out.extend_from_slice(&wrap_marker(0xFF51, &new_siz));
    out.extend_from_slice(&wrap_marker(0xFF52, &new_cod));

    // -- Per-component COC for any kernel differing from the default. --
    for (i, src) in sources.iter().enumerate() {
        let cod = marker_payload(src, find_marker(src, 0xFF52));
        let transform = *cod.last().unwrap();
        if transform != default_transform {
            let spcod = &cod[5..]; // SPcod tail = SPcoc for Csiz < 257
            let mut coc = vec![i as u8, 0x00]; // Ccoc, Scoc
            coc.extend_from_slice(spcod);
            out.extend_from_slice(&wrap_marker(0xFF53, &coc));
        }
    }

    // -- QCD default (component 0); per-component QCC where kernels differ. --
    let qcd0 = marker_payload(base, find_marker(base, 0xFF5C));
    out.extend_from_slice(&wrap_marker(0xFF5C, qcd0));
    for (i, src) in sources.iter().enumerate() {
        let cod = marker_payload(src, find_marker(src, 0xFF52));
        if *cod.last().unwrap() != default_transform {
            let qcd = marker_payload(src, find_marker(src, 0xFF5C));
            let mut qcc = vec![i as u8];
            qcc.extend_from_slice(qcd);
            out.extend_from_slice(&wrap_marker(0xFF5D, &qcc));
        }
    }

    // -- SOT (Psot patched) + SOD + concatenated bodies (CPRL order) + EOC. --
    let sot_off = out.len();
    out.extend_from_slice(&0xFF90u16.to_be_bytes());
    out.extend_from_slice(&10u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // Isot
    out.extend_from_slice(&0u32.to_be_bytes()); // Psot (patched)
    out.push(0); // TPsot
    out.push(1); // TNsot
    out.extend_from_slice(&0xFF93u16.to_be_bytes()); // SOD
    for src in sources {
        out.extend_from_slice(tile_body(src));
    }
    out.extend_from_slice(&0xFFD9u16.to_be_bytes()); // EOC
    let psot = (out.len() - 2 - sot_off) as u32;
    out[sot_off + 6..sot_off + 10].copy_from_slice(&psot.to_be_bytes());
    out
}

/// Three components with *interleaved* kernels — 9-7 / 5-3 / 9-7 — so
/// the reassembly's two lanes carry non-contiguous component sets
/// (9-7 lane = {0, 2}, 5-3 lane = {1}). A lane-tag or in-lane-index slip
/// would place component 2's 9-7 plane where component 1's 5-3 plane
/// belongs (or vice versa); asserting each component against its own
/// single-component decode pins the interleave exactly.
#[test]
fn mixed_kernel_three_components_interleaved_lanes() {
    let ref97 = decode_j2k(GRAY_MK_97).expect("decode 9-7 source");
    let ref53 = decode_j2k(GRAY_MK_53).expect("decode 5-3 source");
    let expected = [
        &ref97.components[0].samples,
        &ref53.components[0].samples,
        &ref97.components[0].samples,
    ];

    let stream = assemble_multi_component(&[GRAY_MK_97, GRAY_MK_53, GRAY_MK_97]);
    let img = decode_j2k(&stream).expect("decode 9-7/5-3/9-7 mixed-kernel stream");
    assert_eq!(img.components.len(), 3);
    for (c, exp) in expected.iter().enumerate() {
        assert_eq!(
            &img.components[c].samples, *exp,
            "component {c} landed in the wrong kernel lane"
        );
    }
    // The 5-3 (lossless) middle plane must differ from the 9-7 (lossy)
    // outer planes, so a lane mix-up could not hide behind identical data.
    assert_ne!(img.components[0].samples, img.components[1].samples);
    assert_eq!(
        img.components[0].samples, img.components[2].samples,
        "the two 9-7 components share a source and must match"
    );
}

/// Minimal binary-PPM (P6, maxval 255) parser → `(w, h, interleaved
/// RGB payload)`.
fn ppm_payload(bytes: &[u8]) -> (usize, usize, &[u8]) {
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
    assert_eq!(toks[0], b"P6");
    let w: usize = std::str::from_utf8(toks[1]).unwrap().parse().unwrap();
    let h: usize = std::str::from_utf8(toks[2]).unwrap().parse().unwrap();
    assert_eq!(toks[3], b"255");
    (w, h, &bytes[i + 1..])
}

/// §B.12.1.5: a CPRL codestream with non-power-of-two sub-sampling
/// (XRsiz = YRsiz = 3) must decode — the power-of-two constraint is
/// stated only for RPCL (§B.12.1.3) and PCRL (§B.12.1.4). Every
/// component plane must match the committed black-box reference decode.
#[test]
fn cprl_non_power_of_two_subsampling_matches_reference() {
    let img = decode_j2k(RGB_CPRL_SUB3_53).expect("decode CPRL with XRsiz=YRsiz=3");
    assert_eq!(img.components.len(), 3);
    let (w, h, rgb) = ppm_payload(RGB_CPRL_SUB3_REF_PPM);
    for (c, comp) in img.components.iter().enumerate() {
        assert_eq!(
            (comp.width as usize, comp.height as usize),
            (w, h),
            "component {c} dimensions differ from the reference"
        );
        let plane: Vec<i32> = (0..w * h).map(|p| rgb[p * 3 + c] as i32).collect();
        assert_eq!(
            comp.samples, plane,
            "component {c} diverged from the reference CPRL decode"
        );
    }
}

/// CPRL, non-power-of-two sub-sampling, with **multiple precincts** per
/// resolution. The §B.12.1.5 sweep now orders several precincts by their
/// reference-grid corners, each scaled by the non-pow2 XRsiz / YRsiz — so
/// this exercises the corner arithmetic the single-precinct case above
/// left trivial. Every component must match the reference decode.
#[test]
fn cprl_non_power_of_two_multi_precinct_matches_reference() {
    let img = decode_j2k(RGB_CPRL_SUB3_MP_53).expect("decode multi-precinct CPRL sub-3");
    assert_eq!(img.components.len(), 3);
    let (w, h, rgb) = ppm_payload(RGB_CPRL_SUB3_MP_REF_PPM);
    for (c, comp) in img.components.iter().enumerate() {
        assert_eq!(
            (comp.width as usize, comp.height as usize),
            (w, h),
            "component {c} dimensions differ from the reference"
        );
        let plane: Vec<i32> = (0..w * h).map(|p| rgb[p * 3 + c] as i32).collect();
        assert_eq!(
            comp.samples, plane,
            "component {c} diverged from the multi-precinct CPRL reference"
        );
    }
}

/// The companion negative: RPCL (§B.12.1.3) *does* require power-of-two
/// sub-sampling. Flipping the assembled CPRL stream's COD progression
/// byte to RPCL (3) while leaving XRsiz = YRsiz = 3 must be rejected.
#[test]
fn rpcl_non_power_of_two_subsampling_is_rejected() {
    // Baseline: the CPRL stream decodes.
    assert!(decode_j2k(RGB_CPRL_SUB3_53).is_ok());
    let mut mutated = RGB_CPRL_SUB3_53.to_vec();
    // COD SGcod progression byte = marker(2) + Lcod(2) + Scod(1) => the
    // first SGcod byte sits 5 past the COD marker.
    let cod_off = find_marker(&mutated, 0xFF52);
    mutated[cod_off + 5] = 0x02; // RPCL (Table A.16)
    assert!(
        decode_j2k(&mutated).is_err(),
        "RPCL requires power-of-two sub-sampling (§B.12.1.3) — non-pow2 must be rejected"
    );
}

/// PCRL (§B.12.1.4) likewise "shall be powers of two": the same non-pow2
/// CPRL stream re-flagged as PCRL must be rejected.
#[test]
fn pcrl_non_power_of_two_subsampling_is_rejected() {
    let mut mutated = RGB_CPRL_SUB3_53.to_vec();
    let cod_off = find_marker(&mutated, 0xFF52);
    mutated[cod_off + 5] = 0x01; // PCRL (Table A.16)
    assert!(
        decode_j2k(&mutated).is_err(),
        "PCRL requires power-of-two sub-sampling (§B.12.1.4) — non-pow2 must be rejected"
    );
}

/// Clean-room assembler: splice two single-component single-tile J2K
/// codestreams with **divergent Table A.19 code-block-style bytes**
/// (e.g. one T.814 HT stream and one Annex D stream) into a single
/// two-component codestream — the T.814 §8.2 HTDECLARED shape when the
/// styles differ on SPcod/SPcoc bit 6.
///
/// Same layout as `assemble_mixed_kernel` (CPRL so component 0's
/// packets precede component 1's; COC + QCC restate component 1 from
/// the second stream — including its own style byte), plus the T.814
/// Annex A signalling: `Rsiz` bit 14 and a `CAP` marker whose `Ccap15`
/// bits 14-15 declare the HTDECLARED set.
fn assemble_mixed_style(base: &[u8], other: &[u8]) -> Vec<u8> {
    let siz_off = find_marker(base, 0xFF51);
    let siz = marker_payload(base, siz_off);
    let csiz_pos = 2 + 4 * 8; // 38
    let csiz = u16::from_be_bytes([siz[csiz_pos], siz[csiz_pos + 1]]);
    assert_eq!(csiz, 1, "assembler expects single-component sources");
    let comp_desc = &siz[csiz_pos + 2..csiz_pos + 5];
    let mut new_siz = Vec::new();
    new_siz.extend_from_slice(&siz[..csiz_pos]);
    // T.814 §A.2: Rsiz bit 14 flags an HTJ2K codestream.
    new_siz[0] |= 0x40;
    new_siz.extend_from_slice(&2u16.to_be_bytes()); // Csiz = 2
    new_siz.extend_from_slice(comp_desc);
    new_siz.extend_from_slice(comp_desc);

    // CAP (T.814 §A.3): Pcap15 + Ccap15 bits 15..14 = 10 (HTDECLARED).
    let mut cap_payload = Vec::new();
    cap_payload.extend_from_slice(&(1u32 << (32 - 15)).to_be_bytes());
    cap_payload.extend_from_slice(&0x8000u16.to_be_bytes());

    // COD: the base stream's, rewritten to CPRL.
    let mut new_cod = marker_payload(base, find_marker(base, 0xFF52)).to_vec();
    new_cod[1] = 0x04; // SGcod progression = CPRL
    assert_eq!(new_cod[4], 0x00, "COD MCT must be off");

    // COC for component 1: SPcoc (incl. its style byte) copied from
    // the other stream's COD.
    let cod_other = marker_payload(other, find_marker(other, 0xFF52));
    let mut coc_payload = vec![0x01u8, 0x00]; // Ccoc = 1, Scoc = 0
    coc_payload.extend_from_slice(&cod_other[5..]);

    // QCD from the base; QCC for component 1 from the other stream.
    let qcd = marker_payload(base, find_marker(base, 0xFF5C));
    let qcd_other = marker_payload(other, find_marker(other, 0xFF5C));
    let mut qcc_payload = vec![0x01u8];
    qcc_payload.extend_from_slice(qcd_other);

    let mut body = Vec::new();
    body.extend_from_slice(tile_body(base));
    body.extend_from_slice(tile_body(other));

    let mut out = Vec::new();
    out.extend_from_slice(&0xFF4Fu16.to_be_bytes()); // SOC
    out.extend_from_slice(&wrap_marker(0xFF51, &new_siz));
    out.extend_from_slice(&wrap_marker(0xFF50, &cap_payload)); // CAP
    out.extend_from_slice(&wrap_marker(0xFF52, &new_cod));
    out.extend_from_slice(&wrap_marker(0xFF53, &coc_payload));
    out.extend_from_slice(&wrap_marker(0xFF5C, qcd));
    out.extend_from_slice(&wrap_marker(0xFF5D, &qcc_payload));
    let sot_off = out.len();
    out.extend_from_slice(&0xFF90u16.to_be_bytes());
    out.extend_from_slice(&10u16.to_be_bytes()); // Lsot
    out.extend_from_slice(&0u16.to_be_bytes()); // Isot
    out.extend_from_slice(&0u32.to_be_bytes()); // Psot (patched)
    out.push(0); // TPsot
    out.push(1); // TNsot
    out.extend_from_slice(&0xFF93u16.to_be_bytes()); // SOD
    out.extend_from_slice(&body);
    out.extend_from_slice(&0xFFD9u16.to_be_bytes()); // EOC
    let psot = (out.len() - 2 - sot_off) as u32;
    out[sot_off + 6..sot_off + 10].copy_from_slice(&psot.to_be_bytes());
    out
}

/// T.814 §8.2 **HTDECLARED**: a tile whose components mix HT and
/// Annex D block coding at tile-component granularity — the `COC`
/// carries a Table A.19 style byte whose bit 6 diverges from the
/// `COD`'s, so each component's §B.10.7 segment split and tier-1
/// dispatch resolve independently. Both orientations (HT first and
/// Annex D first) must reconstruct each component identically to its
/// single-component decode — which is bit-exact to the sources here
/// (both lossless).
#[test]
fn htdeclared_mixed_ht_and_annex_d_components_reconstruct() {
    use oxideav_jpeg2000::encode::{encode_j2k, EncodeParams};
    let mut seed = 0x4854_00A1u32;
    let mut noise = |n: usize| -> Vec<u8> {
        (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 24) as u8
            })
            .collect()
    };
    let pa = noise(32 * 32);
    let pb = noise(32 * 32);
    let base_params = EncodeParams {
        decomposition_levels: 2,
        code_block_exp: (4, 4),
        ..EncodeParams::default()
    };
    let plain = encode_j2k(&[&pa], 32, 32, &base_params).expect("plain encode");
    let ht = encode_j2k(
        &[&pb],
        32,
        32,
        &EncodeParams {
            high_throughput: true,
            ..base_params
        },
    )
    .expect("HT encode");

    let want_a: Vec<i32> = pa.iter().map(|&v| i32::from(v)).collect();
    let want_b: Vec<i32> = pb.iter().map(|&v| i32::from(v)).collect();

    // Annex D component 0, HT component 1.
    let mixed = assemble_mixed_style(&plain, &ht);
    let img = decode_j2k(&mixed).expect("decode HTDECLARED (Annex D + HT)");
    assert_eq!(img.components.len(), 2);
    assert_eq!(img.components[0].samples, want_a, "Annex D component");
    assert_eq!(img.components[1].samples, want_b, "HT component");

    // HT component 0, Annex D component 1.
    let mixed = assemble_mixed_style(&ht, &plain);
    let img = decode_j2k(&mixed).expect("decode HTDECLARED (HT + Annex D)");
    assert_eq!(img.components.len(), 2);
    assert_eq!(img.components[0].samples, want_b, "HT component");
    assert_eq!(img.components[1].samples, want_a, "Annex D component");
}

/// Clean-room tile-lane assembler: from two encodes of the same
/// 2×2-tile image with identical geometry / quantisation — one all
/// Annex D, one all HT — build a single codestream whose tiles mix
/// the two block-coding lanes at **tile** granularity: the T.814 §8.2
/// HTDECLARED set in its §8.5 HETEROGENEOUS shape, where the lane
/// switch rides the §A.6.1 first-tile-part `COD` override rather than
/// a per-component `COC`.
///
/// `main` supplies the main header (its SIZ gains the T.814 §A.2
/// `Rsiz` bit 14 and a §A.3 `CAP` segment taken from
/// `cap_payload_src`; `Ccap15` bit 15 is set to declare HTDECLARED).
/// Every tile listed in `override_tiles` is taken from `other`
/// instead, with `other`'s main-header `COD` payload restated as a
/// tile-part `COD` in its `TPsot = 0` header (Psot re-patched), so
/// that tile's style byte — including SPcod bit 6 — diverges from the
/// main header's.
fn assemble_mixed_tile_lanes(
    main: &[u8],
    other: &[u8],
    cap_payload_src: &[u8],
    override_tiles: &[u16],
) -> Vec<u8> {
    let cs_main = parse_codestream(main).expect("parse main lane");
    let cs_other = parse_codestream(other).expect("parse other lane");
    assert_eq!(cs_main.tile_parts.len(), cs_other.tile_parts.len());

    // Main header: SOC + SIZ (Rsiz |= bit 14) + CAP + the rest.
    let mut out = Vec::new();
    out.extend_from_slice(&0xFF4Fu16.to_be_bytes());
    let siz_off = find_marker(main, 0xFF51);
    let mut siz = marker_payload(main, siz_off).to_vec();
    siz[0] |= 0x40; // Rsiz bit 14: HTJ2K codestream (T.814 §A.2)
    out.extend_from_slice(&wrap_marker(0xFF51, &siz));
    // CAP: reuse the HT lane's payload, with Ccap15 bit 15 set —
    // HTDECLARED (T.814 §A.3 / §8.2).
    let cap_off = find_marker(cap_payload_src, 0xFF50);
    let mut cap = marker_payload(cap_payload_src, cap_off).to_vec();
    cap[4] |= 0x80;
    out.extend_from_slice(&wrap_marker(0xFF50, &cap));
    // The rest of the main header, minus SIZ and any CAP already
    // there (walk marker by marker).
    let mut pos = siz_off + 4 + siz.len();
    let first_sot = cs_main.tile_parts[0].sot_offset;
    while pos < first_sot {
        let marker = u16::from_be_bytes([main[pos], main[pos + 1]]);
        let len = u16::from_be_bytes([main[pos + 2], main[pos + 3]]) as usize;
        if marker != 0xFF50 {
            out.extend_from_slice(&main[pos..pos + 2 + len]);
        }
        pos += 2 + len;
    }
    // The `other` lane's main COD payload becomes the override tiles'
    // tile-part COD.
    let cod_other = marker_payload(other, find_marker(other, 0xFF52)).to_vec();
    // Tile-parts, in codestream order.
    for (i, tp) in cs_main.tile_parts.iter().enumerate() {
        if override_tiles.contains(&tp.sot.tile_index) {
            let tp_o = &cs_other.tile_parts[i];
            assert_eq!(tp_o.sot.tile_index, tp.sot.tile_index);
            assert_eq!(tp_o.sot.tile_part_index, 0, "one tile-part per tile");
            let start = out.len();
            // SOT segment (12 bytes), then the inserted COD, then the
            // rest of the tile-part header + body.
            out.extend_from_slice(&other[tp_o.sot_offset..tp_o.sot_offset + 12]);
            out.extend_from_slice(&wrap_marker(0xFF52, &cod_other));
            out.extend_from_slice(&other[tp_o.sot_offset + 12..tp_o.body_offset + tp_o.body_len]);
            let psot = (out.len() - start) as u32;
            out[start + 6..start + 10].copy_from_slice(&psot.to_be_bytes());
        } else {
            out.extend_from_slice(&main[tp.sot_offset..tp.body_offset + tp.body_len]);
        }
    }
    out.extend_from_slice(&0xFFD9u16.to_be_bytes());
    out
}

/// T.814 §8.2 HTDECLARED at **tile** granularity across a multi-tile
/// grid (the §8.5 HETEROGENEOUS shape): tiles 0 and 3 decode with one
/// block-coding lane, tiles 1 and 2 with the other, the switch riding
/// each override tile's first-tile-part `COD`. Both orientations
/// (HT-main with Annex D override tiles, and Annex-D-main with HT
/// override tiles) must reconstruct the source raster bit-exactly —
/// both lanes are lossless here. Both assembled orientations were
/// additionally verified byte-identical to the source through an
/// independent black-box decoder during development (a second,
/// HT-only opaque decoder declines tile-part `COD` segments
/// altogether, so it cannot arbitrate this shape).
#[test]
fn htdeclared_mixed_lanes_across_tiles_reconstruct() {
    use oxideav_jpeg2000::encode::{encode_j2k, EncodeParams};
    let (w, h) = (64u32, 64u32);
    let mut seed = 0x7411_C0DEu32;
    let src: Vec<u8> = (0..w * h)
        .map(|_| {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 24) as u8
        })
        .collect();
    let base = EncodeParams {
        decomposition_levels: 2,
        code_block_exp: (4, 4),
        tile_size: Some((32, 32)),
        ..EncodeParams::default()
    };
    let plain = encode_j2k(&[&src], w, h, &base).expect("Annex D encode");
    let ht = encode_j2k(
        &[&src],
        w,
        h,
        &EncodeParams {
            high_throughput: true,
            ..base
        },
    )
    .expect("HT encode");
    let want: Vec<i32> = src.iter().map(|&v| i32::from(v)).collect();

    // HT main header; tiles 1 and 2 carry Annex D tile-part CODs.
    let mixed = assemble_mixed_tile_lanes(&ht, &plain, &ht, &[1, 2]);
    let cs = parse_codestream(&mixed).expect("parse mixed lanes");
    assert_eq!(cs.tile_parts.len(), 4);
    let img = decode_j2k(&mixed).expect("decode HT-main mixed-lane grid");
    assert_eq!(img.components[0].samples, want, "HT-main mixed lanes");

    // Annex D main header (gains Rsiz bit 14 + CAP); tiles 1 and 2
    // carry HT tile-part CODs.
    let mixed = assemble_mixed_tile_lanes(&plain, &ht, &ht, &[1, 2]);
    let img = decode_j2k(&mixed).expect("decode AnnexD-main mixed-lane grid");
    assert_eq!(img.components[0].samples, want, "Annex-D-main mixed lanes");
}

/// The per-component style split also carries the Annex D sub-styles:
/// an Annex D component with the §D.6 bypass + §D.4.2 termination
/// styles coexists with a default-style component in one tile, each
/// decoding with its own §B.10.7 segment layout.
#[test]
fn mixed_annex_d_styles_per_component_reconstruct() {
    use oxideav_jpeg2000::encode::{encode_j2k, EncodeParams};
    let mut seed = 0x4854_00B1u32;
    let mut noise = |n: usize| -> Vec<u8> {
        (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 24) as u8
            })
            .collect()
    };
    let pa = noise(32 * 32);
    let pb = noise(32 * 32);
    let base_params = EncodeParams {
        decomposition_levels: 2,
        code_block_exp: (4, 4),
        ..EncodeParams::default()
    };
    let plain = encode_j2k(&[&pa], 32, 32, &base_params).expect("plain encode");
    let styled = encode_j2k(
        &[&pb],
        32,
        32,
        &EncodeParams {
            bypass: true,
            terminate_all: true,
            ..base_params
        },
    )
    .expect("styled encode");

    let mixed = assemble_mixed_style(&plain, &styled);
    let img = decode_j2k(&mixed).expect("decode mixed Annex D styles");
    let want_a: Vec<i32> = pa.iter().map(|&v| i32::from(v)).collect();
    let want_b: Vec<i32> = pb.iter().map(|&v| i32::from(v)).collect();
    assert_eq!(img.components[0].samples, want_a, "default-style component");
    assert_eq!(
        img.components[1].samples, want_b,
        "bypass+termall component"
    );
}

// ---------------------------------------------------------------------------
// ISO/IEC 15444-4-style conformance-corpus depth (round 410): fixtures
// from a real black-box encoder covering C.1 ATS axes the committed
// corpus did not yet pin — non-zero SIZ image/tile offsets, tile-parts
// split by layer, PLT / TLM pointer markers, MCT-off RGB, signed and
// deep bit depths, reference-grid component sub-sampling, and the JP2
// container. Lossless fixtures assert pixel-exactness against the
// regenerated source raster; the 9-7 offset fixture asserts
// byte-exactness against a committed reference decode (two independent
// black-box decoders agree byte-for-byte on it). COM markers scrubbed.
// ---------------------------------------------------------------------------

const GRAY_OFF31_53: &[u8] = include_bytes!("data/gray-33x29-off31-53.j2k");
const GRAY_OFF31_97: &[u8] = include_bytes!("data/gray-33x29-off31-97.j2k");
const GRAY_OFF31_97_REF_PGX: &[u8] = include_bytes!("data/gray-33x29-off31-97-ref.pgx");
const GRAY_OFFTILED_53: &[u8] = include_bytes!("data/gray-96x80-offtiled-53.j2k");
const GRAY_TP_LAYERS_53: &[u8] = include_bytes!("data/gray-64x64-tp-layers-53.j2k");
const GRAY_PLT_53: &[u8] = include_bytes!("data/gray-64x64-plt-53.j2k");
const GRAY_TLM_53: &[u8] = include_bytes!("data/gray-96x80-tlm-53.j2k");
const RGB_MCT0_53: &[u8] = include_bytes!("data/rgb-48x32-mct0-53.j2k");
const GRAY_S8_53: &[u8] = include_bytes!("data/gray-32x32-s8-53.j2k");
const GRAY_S12_53: &[u8] = include_bytes!("data/gray-32x32-s12-53.j2k");
const GRAY_U16_53: &[u8] = include_bytes!("data/gray-32x32-u16-53.j2k");
const RGB_SUB21_53: &[u8] = include_bytes!("data/rgb-95x48-sub21-53.j2k");
const RGB_SUB21_REF0_PGX: &[u8] = include_bytes!("data/rgb-95x48-sub21-53-ref0.pgx");
const RGB_SUB21_REF1_PGX: &[u8] = include_bytes!("data/rgb-95x48-sub21-53-ref1.pgx");
const RGB_SUB21_REF2_PGX: &[u8] = include_bytes!("data/rgb-95x48-sub21-53-ref2.pgx");
const RGB_JP2: &[u8] = include_bytes!("data/rgb-48x32.jp2");

/// The deterministic gray gradient the offset / pointer-marker / PLT /
/// TLM fixtures were encoded from, at an arbitrary size.
fn gray_pattern(w: i32, h: i32) -> Vec<i32> {
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            out.push((x * 7 + y * 13 + (x * y) % 31) % 256);
        }
    }
    out
}

/// Minimal ISO/IEC 15444-4 §B.2.6 PGX reference-image parser (the
/// conformance suite's reference format): `PG ML [+|-]depth w h\n`
/// header, big-endian samples, 1 byte per sample up to depth 8, 2
/// bytes up to 16, sign-extended two's complement when signed.
fn pgx_payload(bytes: &[u8]) -> (bool, u32, usize, usize, Vec<i32>) {
    let nl = bytes
        .iter()
        .position(|&b| b == b'\n')
        .expect("PGX header newline");
    let header = std::str::from_utf8(&bytes[..nl]).expect("PGX header utf8");
    let mut toks = header.split_whitespace();
    assert_eq!(toks.next(), Some("PG"));
    assert_eq!(toks.next(), Some("ML"));
    // The sign may be fused with the depth ("+8") or separate ("+ 8").
    let t = toks.next().expect("depth token");
    let (signed, depth): (bool, u32) = if t == "+" || t == "-" {
        (
            t == "-",
            toks.next().expect("depth").parse().expect("depth"),
        )
    } else {
        let signed = t.starts_with('-');
        (
            signed,
            t.trim_start_matches(['+', '-']).parse().expect("depth"),
        )
    };
    let w: usize = toks.next().expect("width").parse().expect("width");
    let h: usize = toks.next().expect("height").parse().expect("height");
    let raw = &bytes[nl + 1..];
    let mut vals = Vec::with_capacity(w * h);
    if depth <= 8 {
        for &b in &raw[..w * h] {
            vals.push(if signed {
                i32::from(b as i8)
            } else {
                i32::from(b)
            });
        }
    } else {
        for p in raw[..2 * w * h].chunks_exact(2) {
            let u = u16::from_be_bytes([p[0], p[1]]);
            vals.push(if signed {
                i32::from(u as i16)
            } else {
                i32::from(u)
            });
        }
    }
    (signed, depth, w, h, vals)
}

/// §B.2 non-zero image origin: `XOsiz = 3`, `YOsiz = 1` on a 33×29
/// gray raster (lossless 5-3, NL = 2). Equations B-1/B-12 anchor every
/// tile-component region (and the DWT parity) at the offset origin, so
/// pixel-exactness pins the whole reference-grid anchoring chain.
#[test]
fn gray_53_image_offset_is_pixel_exact() {
    let cs = parse_codestream(GRAY_OFF31_53).expect("parse");
    assert_eq!((cs.header.siz.x_offset, cs.header.siz.y_offset), (3, 1));
    let img = decode_j2k(GRAY_OFF31_53).expect("decode offset stream");
    assert_eq!((img.width, img.height), (33, 29));
    assert_eq!(img.components[0].samples, gray_pattern(33, 29));
}

/// The same offset geometry through the 9-7 irreversible path, pinned
/// byte-exact against a committed §B.2.6 PGX reference decode (both
/// independent black-box decoders reconstruct this stream
/// identically).
#[test]
fn gray_97_image_offset_matches_black_box_reference() {
    let (signed, depth, rw, rh, refv) = pgx_payload(GRAY_OFF31_97_REF_PGX);
    assert!(!signed);
    assert_eq!(depth, 8);
    assert_eq!((rw, rh), (33, 29));
    let img = decode_j2k(GRAY_OFF31_97).expect("decode 9-7 offset stream");
    assert_eq!(img.components[0].samples, refv);
}

/// Image origin offset (5, 5) *and* tile origin offset (2, 3) with a
/// 32×24 tile grid over 96×80: the §B.3 / Equation B-7 tile partition
/// anchors at `XTOsiz`, the first tile row/column is cropped by the
/// image origin, and every tile decodes on its absolute-parity grid.
#[test]
fn gray_53_image_and_tile_offsets_with_tiles_is_pixel_exact() {
    let cs = parse_codestream(GRAY_OFFTILED_53).expect("parse");
    assert_eq!((cs.header.siz.x_offset, cs.header.siz.y_offset), (5, 5));
    assert_eq!(
        (cs.header.siz.tile_x_offset, cs.header.siz.tile_y_offset),
        (2, 3)
    );
    assert!(cs.tile_parts.len() > 1, "expected a multi-tile grid");
    let img = decode_j2k(GRAY_OFFTILED_53).expect("decode offset+tiled stream");
    assert_eq!((img.width, img.height), (96, 80));
    assert_eq!(img.components[0].samples, gray_pattern(96, 80));
}

/// §A.4.2 tile-parts split on the *layer* axis (`TPsot` 0, 1, 2 for
/// one tile): three quality layers, each layer's packets in their own
/// tile-part. The SOT walk must chain the parts in `TPsot` order and
/// the final (lossless) layer must land pixel-exact.
#[test]
fn gray_53_tile_parts_by_layer_is_pixel_exact() {
    let cs = parse_codestream(GRAY_TP_LAYERS_53).expect("parse");
    assert!(cs.header.cod.layers >= 3, "fixture must carry 3 layers");
    assert!(
        cs.tile_parts.iter().any(|tp| tp.sot.tile_part_index > 0),
        "fixture must carry TPsot > 0 tile-parts"
    );
    let img = decode_j2k(GRAY_TP_LAYERS_53).expect("decode layer-split tile-parts");
    assert_eq!(img.components[0].samples, gray_pattern(64, 64));
}

/// §A.7.3 PLT (packet-length, tile-part header) pointer marker: the
/// tile-part header carries packet lengths the decoder does not need
/// but must parse-and-carry without desynchronising the header walk.
#[test]
fn gray_53_plt_pointer_marker_is_pixel_exact() {
    let cs = parse_codestream(GRAY_PLT_53).expect("parse");
    assert!(
        cs.tile_parts.iter().any(|tp| tp
            .markers
            .iter()
            .any(|m| matches!(m, oxideav_jpeg2000::TilePartMarker::Plt(_)))),
        "fixture must carry a PLT marker"
    );
    let img = decode_j2k(GRAY_PLT_53).expect("decode PLT stream");
    assert_eq!(img.components[0].samples, gray_pattern(64, 64));
}

/// §A.7.1 TLM (tile-part length, main header) pointer marker over a
/// 48×40 tile grid: the main header announces every tile-part length
/// up front; the sequential SOT walk must agree with it.
#[test]
fn gray_53_tlm_pointer_marker_with_tiles_is_pixel_exact() {
    let img = decode_j2k(GRAY_TLM_53).expect("decode TLM stream");
    assert_eq!((img.width, img.height), (96, 80));
    assert_eq!(img.components[0].samples, gray_pattern(96, 80));
}

/// Three-component RGB with the MCT explicitly OFF (`SGcod` MCT = 0):
/// each plane codes independently through the reversible path — no
/// §G.2 RCT on decode.
#[test]
fn rgb_53_mct_off_is_pixel_exact() {
    let cs = parse_codestream(RGB_MCT0_53).expect("parse");
    assert_eq!(cs.header.cod.multi_component_transform, 0);
    let img = decode_j2k(RGB_MCT0_53).expect("decode MCT-off RGB");
    assert_eq!(img.components.len(), 3);
    let want: [Vec<i32>; 3] = {
        let (w, h) = (48i32, 32i32);
        let mut r = Vec::new();
        let mut g = Vec::new();
        let mut b = Vec::new();
        for y in 0..h {
            for x in 0..w {
                r.push((x * 5 + y * 11) % 256);
                g.push((x * 9 + y * 3) % 256);
                b.push((x * 2 + y * 7) % 256);
            }
        }
        [r, g, b]
    };
    for (c, exp) in img.components.iter().zip(want.iter()) {
        assert_eq!(&c.samples, exp);
    }
}

/// Signed 8-bit samples (SIZ `Ssiz` sign bit set): no DC level shift
/// on decode (§G.1.2 applies only to unsigned components), two's
/// complement range −128..=127 reproduced exactly.
#[test]
fn gray_53_signed_8bit_is_pixel_exact() {
    let cs = parse_codestream(GRAY_S8_53).expect("parse");
    assert!(cs.header.siz.components[0].is_signed);
    let img = decode_j2k(GRAY_S8_53).expect("decode signed 8-bit");
    let c = &img.components[0];
    assert!(c.is_signed);
    assert_eq!(c.precision_bits, 8);
    let want: Vec<i32> = gray_pattern(32, 32).iter().map(|&v| v - 128).collect();
    assert_eq!(c.samples, want);
}

/// Signed 12-bit samples: the deep signed lane (range −2048..=2047)
/// through the reversible path.
#[test]
fn gray_53_signed_12bit_is_pixel_exact() {
    let img = decode_j2k(GRAY_S12_53).expect("decode signed 12-bit");
    let c = &img.components[0];
    assert!(c.is_signed);
    assert_eq!(c.precision_bits, 12);
    let (w, h) = (32i32, 32i32);
    let mut want = Vec::new();
    for y in 0..h {
        for x in 0..w {
            want.push((x * 31 + y * 17 + (x * y) % 211) % 4096 - 2048);
        }
    }
    assert_eq!(c.samples, want);
}

/// Unsigned 16-bit samples through the reversible path (full-depth
/// magnitude lane + 16-bit output surface).
#[test]
fn gray_53_unsigned_16bit_is_pixel_exact() {
    let img = decode_j2k(GRAY_U16_53).expect("decode 16-bit");
    let c = &img.components[0];
    assert!(!c.is_signed);
    assert_eq!(c.precision_bits, 16);
    let (w, h) = (32i32, 32i32);
    let mut want = Vec::new();
    for y in 0..h {
        for x in 0..w {
            want.push((x * 997 + y * 271 + (x * y) % 4099) % 65536);
        }
    }
    assert_eq!(c.samples, want);
}

/// §B.2 reference-grid component sub-sampling on every component
/// (`XRsiz = 2`, `YRsiz = 1`, Xsiz = 95): each 48×48 plane decodes on
/// its own component grid (`ceil(95 / 2) = 48` columns), pinned
/// byte-exact against committed §B.2.6 PGX reference decodes.
#[test]
fn rgb_53_all_component_subsampling_matches_black_box_reference() {
    let cs = parse_codestream(RGB_SUB21_53).expect("parse");
    for comp in &cs.header.siz.components {
        assert_eq!((comp.h_separation, comp.v_separation), (2, 1));
    }
    let img = decode_j2k(RGB_SUB21_53).expect("decode sub-sampled RGB");
    assert_eq!(img.components.len(), 3);
    for (c, refbytes) in
        img.components
            .iter()
            .zip([RGB_SUB21_REF0_PGX, RGB_SUB21_REF1_PGX, RGB_SUB21_REF2_PGX])
    {
        let (_signed, _depth, rw, rh, refv) = pgx_payload(refbytes);
        assert_eq!((c.width as usize, c.height as usize), (rw, rh));
        assert_eq!((rw, rh), (48, 48));
        assert_eq!(c.samples, refv);
    }
}

/// JP2 container from a real black-box encoder: locate the `jp2c`
/// codestream through `jp2::parse_jp2` and decode it pixel-exact.
#[test]
fn jp2_container_rgb_is_pixel_exact() {
    let container = oxideav_jpeg2000::jp2::parse_jp2(RGB_JP2).expect("parse jp2");
    let cs = &RGB_JP2
        [container.codestream_offset..container.codestream_offset + container.codestream_len];
    let img = decode_j2k(cs).expect("decode jp2 codestream");
    assert_eq!(img.components.len(), 3);
    assert_eq!((img.width, img.height), (48, 32));
    // Same planes as the raw-codestream MCT-default fixture family.
    let (w, h) = (48i32, 32i32);
    for (ci, c) in img.components.iter().enumerate() {
        let mut want = Vec::new();
        for y in 0..h {
            for x in 0..w {
                want.push(match ci {
                    0 => (x * 5 + y * 11) % 256,
                    1 => (x * 9 + y * 3) % 256,
                    _ => (x * 2 + y * 7) % 256,
                });
            }
        }
        assert_eq!(&c.samples, &want, "jp2 component {ci}");
    }
}

// Palettized JP2 (T.800 §I.5.3.4 / §I.5.3.5): a 32×24 single-component
// index plane (values 0..=15, black-box lossless 5-3 encode, COM
// markers scrubbed) wrapped with a 16-entry three-column 8-bit `pclr`
// and a `cmap` applying columns 0/1/2 of component 0 as channels
// R/G/B. The committed reference is an opaque black-box JP2 reader's
// decode of the same file (comment-scrubbed P6 PPM) — that reader
// expands the palette exactly as §I.5.3.5 requires, so `decode_jp2`
// must match it byte-for-byte.
const PAL_JP2: &[u8] = include_bytes!("data/pal-32x24.jp2");
const PAL_JP2_REF_PPM: &[u8] = include_bytes!("data/pal-32x24-ref.ppm");

// Channel-Definition JP2 (T.800 §I.5.3.6): a 16×16 three-component
// codestream whose planes are stored in B, G, R order, wrapped with a
// `cdef` associating channel 0 with colour 3 (blue), channel 1 with
// colour 2 (green) and channel 2 with colour 1 (red). The reference
// black-box JP2 reader reorders the decoded channels into colour
// order (R, G, B), matching the `decode_jp2` presentation rule.
const BGR_CDEF_JP2: &[u8] = include_bytes!("data/bgr-cdef-16x16.jp2");
const BGR_CDEF_JP2_REF_PPM: &[u8] = include_bytes!("data/bgr-cdef-16x16-ref.ppm");

/// Palettized JP2 end-to-end: `decode_jp2` expands the single index
/// component through the `pclr` / `cmap` boxes into three 8-bit
/// channels, byte-exact against the black-box reference decode.
#[test]
fn jp2_palette_expansion_matches_reference() {
    let container = oxideav_jpeg2000::jp2::parse_jp2(PAL_JP2).expect("parse palettized jp2");
    let header = &container.header;
    let pclr = header.pclr.as_ref().expect("pclr parsed");
    assert_eq!(pclr.columns.len(), 3);
    assert_eq!(pclr.entries(), 16);
    assert_eq!(header.cmap.as_ref().map(Vec::len), Some(3));

    let img = oxideav_jpeg2000::jp2::decode_jp2(PAL_JP2).expect("decode palettized jp2");
    assert_eq!(img.components.len(), 3, "palette generates 3 channels");
    let (w, h, rgb) = ppm_payload(PAL_JP2_REF_PPM);
    assert_eq!((w, h), (32, 24));
    for (ci, c) in img.components.iter().enumerate() {
        assert_eq!((c.width as usize, c.height as usize), (w, h));
        assert_eq!(c.precision_bits, 8);
        assert!(!c.is_signed);
        let want: Vec<i32> = rgb[ci..].iter().step_by(3).map(|&v| i32::from(v)).collect();
        assert_eq!(c.samples, want, "palette channel {ci}");
    }
    // The interleaved entry point sniffs the JP2 signature and must
    // reproduce the reference PPM payload directly.
    let interleaved = oxideav_jpeg2000::decode_jpeg2000(PAL_JP2).expect("interleaved");
    assert_eq!(interleaved.as_slice(), &rgb[..w * h * 3]);
}

/// Channel-Definition JP2 end-to-end: `decode_jp2` reorders the
/// stored B, G, R planes into colour order per the `cdef` box,
/// byte-exact against the black-box reference decode.
#[test]
fn jp2_cdef_channel_order_matches_reference() {
    let container = oxideav_jpeg2000::jp2::parse_jp2(BGR_CDEF_JP2).expect("parse cdef jp2");
    let defs = container.header.cdef.as_ref().expect("cdef parsed");
    assert_eq!(defs.len(), 3);
    assert_eq!(defs[0].association, 3);

    let img = oxideav_jpeg2000::jp2::decode_jp2(BGR_CDEF_JP2).expect("decode cdef jp2");
    let (w, h, rgb) = ppm_payload(BGR_CDEF_JP2_REF_PPM);
    assert_eq!((w, h), (16, 16));
    assert_eq!(img.components.len(), 3);
    for (ci, c) in img.components.iter().enumerate() {
        let want: Vec<i32> = rgb[ci..].iter().step_by(3).map(|&v| i32::from(v)).collect();
        assert_eq!(c.samples, want, "cdef-ordered channel {ci}");
    }
    // Raw codestream decode (no box layer) still yields the stored
    // B, G, R order — the reorder is purely the §I.5.3.6 box's doing.
    let cs = &BGR_CDEF_JP2
        [container.codestream_offset..container.codestream_offset + container.codestream_len];
    let raw = decode_j2k(cs).expect("decode raw codestream");
    assert_eq!(raw.components[0].samples, img.components[2].samples);
    assert_eq!(raw.components[2].samples, img.components[0].samples);
}

// Precinct-unaligned-tile regression fixtures (round 416). A 320-case
// black-box 5-3 lossless sweep (2 rasters × 5 progression orders ×
// {untiled, 17×15 tiles} × {default, 8×8 code-blocks} × {default,
// 16×16 precincts} × {1, 3 layers} × {no offset, XOsiz/YOsiz = 3/5})
// plus an 80-case 15×13-tile round (both kernels, 8×8 / 16×16
// precincts, 4×4 code-blocks) exposed two decode bugs this crate had
// through round 415, both specific to tiles whose reference-grid edges
// are not precinct-aligned:
//
// * The §B.12.1.3–5 position-keyed orders keyed a **partial first
//   precinct** on `trx0 · 2^(NL − r) · XRsiz` — but the spec's `for x`
//   loop fires its OR-clause at exactly `x = tx0`, the same value for
//   every (component, resolution), so the per-component ceiling
//   rounding mis-ordered the packet walk (all five orders, and every
//   order under an image-origin offset).
// * The §B.6 precinct partition anchor was re-derived from each
//   sub-band's own lo edge; when the level edge sits just below a cell
//   boundary (e.g. trx0 = 15, PPx = 4: level cell 0, band edge 8 in
//   band cell 1) the first precinct silently claimed the next cell's
//   code-blocks (every progression order, tiles + custom precincts).
//
// After the fix the full sweep decodes 5-3 byte-exact against the
// sources and 9-7 byte-exact against at least one of two independent
// reference decoders on every case. These three committed fixtures pin
// the sweep's hardest shapes (COM markers scrubbed):
//
// * 33×29 gray, XOsiz/YOsiz = 3/5, 17×15 tiles, 16×16 precincts, 8×8
//   code-blocks, PCRL, 3 layers.
// * 48×40 RGB (MCT on), 17×15 tiles, CPRL — the multi-tile CPRL shape.
// * 45×39 gray, 15×13 tiles (trx0 = 15 hits the anchor-rounding cell
//   edge), 16×16 precincts, 4×4 code-blocks, RPCL.
const GRAY_TP17_PREC16_PCRL_53: &[u8] =
    include_bytes!("data/gray-33x29-off35-t17-prec16-pcrl-l3-53.j2k");
const RGB_T17_CPRL_53: &[u8] = include_bytes!("data/rgb-48x40-t17-cprl-53.j2k");
const GRAY_T15_PREC16_RPCL_53: &[u8] = include_bytes!("data/gray-45x39-t15-prec16-rpcl-53.j2k");

/// Precinct-unaligned tiles + offsets + position order + layers: the
/// round-416 sweep's hardest gray shape decodes bit-exact.
#[test]
fn unaligned_tiles_precincts_pcrl_offsets_layers_decode() {
    let img = decode_j2k(GRAY_TP17_PREC16_PCRL_53).expect("decode PCRL unaligned tiles");
    assert_eq!((img.width, img.height), (33, 29));
    let c = &img.components[0];
    for y in 0..29i32 {
        for x in 0..33i32 {
            assert_eq!(
                c.samples[(y * 33 + x) as usize],
                (x * 7 + y * 13) % 256,
                "pixel ({x}, {y})"
            );
        }
    }
}

/// Multi-tile CPRL (the §B.12.1.5 component-major order across a tile
/// grid) decodes bit-exact.
#[test]
fn multi_tile_cprl_decodes() {
    let img = decode_j2k(RGB_T17_CPRL_53).expect("decode multi-tile CPRL");
    assert_eq!((img.width, img.height), (48, 40));
    assert_eq!(img.components.len(), 3);
    for (ci, c) in img.components.iter().enumerate() {
        for y in 0..40i32 {
            for x in 0..48i32 {
                let want = match ci {
                    0 => (x * 5 + y * 11) % 256,
                    1 => (x * 9 + y * 3) % 256,
                    _ => (x * 2 + y * 7) % 256,
                };
                assert_eq!(
                    c.samples[(y * 48 + x) as usize],
                    want,
                    "comp {ci} pixel ({x}, {y})"
                );
            }
        }
    }
}

/// The §B.6 anchor-projection cell-edge case: 15×13 tiles put tile 1's
/// full-resolution left edge at trx0 = 15 with 16×16 precincts, where
/// the sub-band's ceiling-divided edge rounds into the next partition
/// cell. Decodes bit-exact after the anchor fix.
#[test]
fn precinct_anchor_cell_edge_tiles_decode() {
    let img = decode_j2k(GRAY_T15_PREC16_RPCL_53).expect("decode 15x13-tile RPCL");
    assert_eq!((img.width, img.height), (45, 39));
    let c = &img.components[0];
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

// §A.4.2 tile-part interleaving: a 64×48 gray lossless 5-3 raster on a
// 2×2 grid of 32×24 tiles, three tile-parts per tile (split on the
// resolution axis, TNsot = 3), transcoded so the twelve tile-parts are
// **interleaved across tiles** — t0p0, t1p0, t2p0, t3p0, t0p1, … —
// instead of the encoder's contiguous per-tile chains. §A.4.2 allows
// exactly this ("tile-parts from other tiles may be interleaved in the
// codestream") as long as each tile's own TPsot sequence stays in
// order. The black-box reference decoder reconstructs the identical
// raster from both orderings (verified during corpus generation); the
// source raster is the arithmetic pattern regenerated below. COM
// markers scrubbed.
const GRAY_TP_INTERLEAVED_53: &[u8] = include_bytes!("data/gray-64x48-tp-interleaved-53.j2k");

/// §A.4.2: tile-parts interleaved across tiles must decode bit-exact —
/// grouping by tile has to reassemble each tile's ascending-TPsot
/// chain from the round-robin codestream layout.
#[test]
fn tile_parts_interleaved_across_tiles_decode() {
    // Prove the fixture really interleaves: the tile indices must not
    // be grouped contiguously.
    let cs = parse_codestream(GRAY_TP_INTERLEAVED_53).expect("parse");
    let order: Vec<(u16, u8)> = cs
        .tile_parts
        .iter()
        .map(|tp| (tp.sot.tile_index, tp.sot.tile_part_index))
        .collect();
    assert_eq!(order.len(), 12, "4 tiles x 3 tile-parts");
    assert_eq!(order[0], (0, 0));
    assert_eq!(order[1], (1, 0), "second tile-part belongs to tile 1");
    assert_eq!(order[4], (0, 1), "tile 0 resumes after the round-robin");
    for tp in &cs.tile_parts {
        assert_eq!(tp.sot.num_tile_parts, 3, "TNsot = 3 in every header");
    }

    let img = decode_j2k(GRAY_TP_INTERLEAVED_53).expect("decode interleaved tile-parts");
    assert_eq!((img.width, img.height), (64, 48));
    let c = &img.components[0];
    for y in 0..48i32 {
        for x in 0..64i32 {
            assert_eq!(
                c.samples[(y * 64 + x) as usize],
                (x * 7 + y * 13) % 256,
                "pixel ({x}, {y})"
            );
        }
    }
}

/// Reassemble the fixture's codestream with the tile-part spans in a
/// caller-chosen order (the main header and `EOC` are preserved).
fn reorder_tile_parts(bytes: &[u8], order: &[usize]) -> Vec<u8> {
    let cs = parse_codestream(bytes).expect("parse for reorder");
    let first_sot = cs.tile_parts[0].sot_offset;
    let mut out = bytes[..first_sot].to_vec();
    for &i in order {
        let tp = &cs.tile_parts[i];
        out.extend_from_slice(&bytes[tp.sot_offset..tp.body_offset + tp.body_len]);
    }
    out.extend_from_slice(&[0xFF, 0xD9]); // EOC
    out
}

/// §A.4.2 / Table A.5 / Table A.6 ordering faults are rejected rather
/// than silently re-sorted: an out-of-order TPsot within a tile, a
/// duplicated TPsot, and a TNsot that misstates the tile's tile-part
/// count all name a lost or mis-assembled tile-part chain.
#[test]
fn tile_part_order_faults_are_rejected() {
    use oxideav_jpeg2000::Error;
    let cs = parse_codestream(GRAY_TP_INTERLEAVED_53).expect("parse");
    // Codestream layout is t0p0 t1p0 t2p0 t3p0 t0p1 t1p1 … (see the
    // interleaving test). The identity order must keep decoding.
    let identity: Vec<usize> = (0..cs.tile_parts.len()).collect();
    let same = reorder_tile_parts(GRAY_TP_INTERLEAVED_53, &identity);
    assert_eq!(
        decode_j2k(&same)
            .expect("identity reorder decodes")
            .components[0]
            .samples,
        decode_j2k(GRAY_TP_INTERLEAVED_53).expect("base").components[0].samples
    );
    // Swap tile 0's TPsot = 1 (index 4) and TPsot = 2 (index 8):
    // tile 0's chain reads 0, 2, 1 — out of order per §A.4.2.
    let mut order = identity.clone();
    order.swap(4, 8);
    let swapped = reorder_tile_parts(GRAY_TP_INTERLEAVED_53, &order);
    assert_eq!(decode_j2k(&swapped), Err(Error::InvalidTilePartIndex));
    // Duplicate TPsot: overwrite tile 0 part 2's TPsot byte (at
    // SOT + 10) with 1.
    let tp = &cs.tile_parts[8];
    assert_eq!((tp.sot.tile_index, tp.sot.tile_part_index), (0, 2));
    let mut dup = GRAY_TP_INTERLEAVED_53.to_vec();
    dup[tp.sot_offset + 10] = 1;
    assert_eq!(decode_j2k(&dup), Err(Error::InvalidTilePartIndex));
    // TNsot misstatement: claim tile 0 has 2 tile-parts (byte at
    // SOT + 11) while three are present.
    let mut wrong = GRAY_TP_INTERLEAVED_53.to_vec();
    wrong[cs.tile_parts[0].sot_offset + 11] = 2;
    assert_eq!(decode_j2k(&wrong), Err(Error::InvalidTilePartIndex));
}

// ---------------------------------------------------------------------------
// ISO/IEC 15444-4 §B.2.3 reduced-resolution decode (`decode_j2k_reduced`):
// the conformance suite's Class-0 reference images are produced by
// discarding N inverse wavelet transforms (the `rN` suffix in its
// reference-file names). Every reversible case below is byte-exact
// against a committed black-box reference decode at the same
// reduction; the 9-7 case carries the usual ±1 half-integer
// inter-decoder latitude.
// ---------------------------------------------------------------------------

use oxideav_jpeg2000::decode_j2k_reduced;

const GRAY_MULTILAYER_R1_REF: &[u8] = include_bytes!("data/gray-64x64-multilayer-53-r1-ref.pgx");
const GRAY_OFFTILED_R2_REF: &[u8] = include_bytes!("data/gray-96x80-offtiled-53-r2-ref.pgx");
const GRAY_OFF31_R1_REF: &[u8] = include_bytes!("data/gray-33x29-off31-53-r1-ref.pgx");
const GRAY_97_FULL_R1_REF: &[u8] = include_bytes!("data/gray-32x32-97full-r1-ref.pgx");
const RGB_RCT_R1_REF0: &[u8] = include_bytes!("data/rgb-16x16-rct-53-r1-ref0.pgx");
const RGB_RCT_R1_REF1: &[u8] = include_bytes!("data/rgb-16x16-rct-53-r1-ref1.pgx");
const RGB_RCT_R1_REF2: &[u8] = include_bytes!("data/rgb-16x16-rct-53-r1-ref2.pgx");

/// Shared body: decode at `discard` levels and compare one gray
/// component byte-exactly against a §B.2.6 PGX reference decode.
fn assert_reduced_matches_pgx(j2k: &[u8], discard: u8, ref_pgx: &[u8], what: &str) {
    let (_signed, _depth, rw, rh, refv) = pgx_payload(ref_pgx);
    let img = decode_j2k_reduced(j2k, discard).expect("reduced decode");
    assert_eq!(img.components.len(), 1);
    let c = &img.components[0];
    assert_eq!(
        (c.width as usize, c.height as usize),
        (rw, rh),
        "{what}: reduced dims"
    );
    assert_eq!(c.samples, refv, "{what}: reduced samples");
}

#[test]
fn reduced_zero_discard_equals_full_decode() {
    let full = decode_j2k(GRAY_MULTILAYER_53).expect("full");
    let red = decode_j2k_reduced(GRAY_MULTILAYER_53, 0).expect("r0");
    assert_eq!(full.components[0].samples, red.components[0].samples);
    assert_eq!((full.width, full.height), (red.width, red.height));
}

#[test]
fn reduced_multilayer_r1_matches_black_box_reference() {
    // 64×64, NL = 2, five quality layers → 32×32 at r1. The tier-2
    // walk still parses every layer's packets for the discarded level.
    assert_reduced_matches_pgx(
        GRAY_MULTILAYER_53,
        1,
        GRAY_MULTILAYER_R1_REF,
        "multilayer r1",
    );
}

#[test]
fn reduced_offsets_and_tiles_r2_matches_black_box_reference() {
    // Image origin (5, 5), tile origin (2, 3), 32×24 tiles → at r2
    // every tile-component corner maps through the Equation B-14
    // ceiling division and the reduced planes must tile the reduced
    // image area gap-free.
    assert_reduced_matches_pgx(GRAY_OFFTILED_53, 2, GRAY_OFFTILED_R2_REF, "offset+tiled r2");
}

#[test]
fn reduced_image_offset_r1_matches_black_box_reference() {
    // XOsiz = 3 / YOsiz = 1 on 33×29: the reduced grid is
    // ceil(36/2) − ceil(3/2) = 16 wide by ceil(30/2) − ceil(1/2) = 14
    // high — the offset ceiling-division path.
    let (_s, _d, rw, rh, _v) = pgx_payload(GRAY_OFF31_R1_REF);
    assert_eq!((rw, rh), (16, 14));
    assert_reduced_matches_pgx(GRAY_OFF31_53, 1, GRAY_OFF31_R1_REF, "offset r1");
}

#[test]
fn reduced_rct_r1_matches_black_box_reference() {
    // The §G.2.2 inverse RCT runs on the reduced planes (all three
    // components share the reduced grid).
    let img = decode_j2k_reduced(RGB_RCT_53, 1).expect("reduced RCT decode");
    assert_eq!(img.components.len(), 3);
    for (c, refbytes) in
        img.components
            .iter()
            .zip([RGB_RCT_R1_REF0, RGB_RCT_R1_REF1, RGB_RCT_R1_REF2])
    {
        let (_s, _d, rw, rh, refv) = pgx_payload(refbytes);
        assert_eq!((c.width as usize, c.height as usize), (rw, rh));
        assert_eq!((rw, rh), (8, 8));
        assert_eq!(c.samples, refv);
    }
}

#[test]
fn reduced_97_r1_tracks_black_box_reference() {
    // 9-7 at r1: the truncated synthesis feeds the same f32
    // integerisation, so the ±1 half-integer inter-decoder latitude
    // applies exactly as at full resolution (ISO/IEC 15444-4
    // Table C.1 budgets far more).
    let (_s, _d, rw, rh, refv) = pgx_payload(GRAY_97_FULL_R1_REF);
    let img = decode_j2k_reduced(GRAY_97_FULL, 1).expect("reduced 9-7 decode");
    let c = &img.components[0];
    assert_eq!((c.width as usize, c.height as usize), (rw, rh));
    let mut peak = 0i32;
    let mut sq = 0u64;
    for (&o, &r) in c.samples.iter().zip(refv.iter()) {
        let d = (o - r).abs();
        peak = peak.max(d);
        sq += (d as u64) * (d as u64);
    }
    assert!(peak <= 1, "reduced 9-7 peak {peak} (> 1)");
    let mse = sq as f64 / refv.len() as f64;
    assert!(mse <= 0.01, "reduced 9-7 MSE {mse} (> 0.01)");
}

#[test]
fn reduced_below_component_levels_is_rejected() {
    use oxideav_jpeg2000::Error;
    // The multilayer fixture carries NL = 2 — discarding 3 levels is
    // unrepresentable and must surface InvalidDecompositionLevels
    // rather than clamp silently.
    let cs = parse_codestream(GRAY_MULTILAYER_53).expect("parse");
    let nl = cs.header.cod.decomposition_levels;
    assert_eq!(
        decode_j2k_reduced(GRAY_MULTILAYER_53, nl + 1),
        Err(Error::InvalidDecompositionLevels)
    );
    // Discarding exactly NL levels (down to the NLLL band) is valid.
    let img = decode_j2k_reduced(GRAY_MULTILAYER_53, nl).expect("LL-only decode");
    assert_eq!((img.width, img.height), (16, 16));
}

// ---------------------------------------------------------------------------
// Layer-limited decode (`decode_j2k_layers`): the layer-progressive
// counterpart of the §B.2.3 reduced-resolution surface. Each
// code-block decodes exactly the coding passes its first `max_layers`
// layers carried — the truncated-reconstruction shape (per-coefficient
// `Nb(u, v)` midpoint lift) — and the committed verdicts are
// byte-exact against black-box reference decodes at the same layer
// limit.
// ---------------------------------------------------------------------------

use oxideav_jpeg2000::decode_j2k_layers;

const GRAY_MULTILAYER_L1_REF: &[u8] = include_bytes!("data/gray-64x64-multilayer-53-l1-ref.pgx");
const GRAY_MULTILAYER_L3_REF: &[u8] = include_bytes!("data/gray-64x64-multilayer-53-l3-ref.pgx");

#[test]
fn layer_limited_prefixes_match_black_box_references() {
    // Five-layer fixture at l = 1 and l = 3: the truncated §E.1.2.1
    // reconstruction (reversible path, Nb < Mb midpoint lift) must be
    // byte-exact against the black-box decode of the same prefix.
    for (limit, refbytes) in [(1u16, GRAY_MULTILAYER_L1_REF), (3, GRAY_MULTILAYER_L3_REF)] {
        let (_s, _d, rw, rh, refv) = pgx_payload(refbytes);
        assert_eq!((rw, rh), (64, 64));
        let img = decode_j2k_layers(GRAY_MULTILAYER_53, limit).expect("layer-limited decode");
        assert_eq!(img.components[0].samples, refv, "layer limit {limit}");
    }
}

#[test]
fn layer_limited_quality_is_monotone_and_saturates() {
    // MSE against the lossless full decode must not increase with the
    // layer count, and a limit at/above the stream's 5 layers decodes
    // identically to decode_j2k.
    let full = decode_j2k(GRAY_MULTILAYER_53).expect("full");
    let fullv = &full.components[0].samples;
    let mut last_mse = f64::INFINITY;
    for l in 1..=5u16 {
        let img = decode_j2k_layers(GRAY_MULTILAYER_53, l).expect("limited");
        let mse = img.components[0]
            .samples
            .iter()
            .zip(fullv.iter())
            .map(|(&a, &b)| ((a - b) as f64).powi(2))
            .sum::<f64>()
            / fullv.len() as f64;
        assert!(
            mse <= last_mse,
            "MSE must be monotone non-increasing (l={l}: {mse} > {last_mse})"
        );
        last_mse = mse;
    }
    assert_eq!(last_mse, 0.0, "all 5 layers must reconstruct losslessly");
    let over = decode_j2k_layers(GRAY_MULTILAYER_53, u16::MAX).expect("over-limit");
    assert_eq!(&over.components[0].samples, fullv);
}

#[test]
fn layer_limited_tile_parts_by_layer_is_consistent() {
    // The tile-part-per-layer fixture: limiting to 1 / 2 layers drops
    // whole tile-parts' worth of packets; the walk must stay in sync
    // (verified byte-exact against the black-box decode during
    // corpus generation) and 3 layers must reproduce the lossless
    // raster.
    for l in 1..=2u16 {
        decode_j2k_layers(GRAY_TP_LAYERS_53, l).expect("layer-limited tile-part decode");
    }
    let img = decode_j2k_layers(GRAY_TP_LAYERS_53, 3).expect("all layers");
    assert_eq!(img.components[0].samples, gray_pattern(64, 64));
}

#[test]
fn layer_limited_zero_is_rejected() {
    use oxideav_jpeg2000::Error;
    assert_eq!(
        decode_j2k_layers(GRAY_MULTILAYER_53, 0),
        Err(Error::InvalidMarkerLength)
    );
}
