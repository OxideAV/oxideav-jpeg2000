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
    // must track the committed black-box reference within the Annex F
    // ±1 floating-point latitude.
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
    let (rw, rh, payload) = pgm_payload(GRAY_BYPASS_97_REF_PGM);
    assert_eq!((rw, rh), (40, 40));
    assert_eq!(payload.len(), c.samples.len());
    let mut max_diff = 0i32;
    for (&ours, &refv) in c.samples.iter().zip(payload.iter()) {
        max_diff = max_diff.max((ours - refv as i32).abs());
    }
    assert!(
        max_diff <= 1,
        "9-7 bypass decode deviates from the reference by {max_diff} (> 1)"
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

/// §D.4.2 predictable-termination error resilience: a codestream that
/// *signals* predictable termination (Table A.19 bit 4) but whose
/// codeword segments were not flushed by the §D.4.2 procedure must be
/// rejected — the decoder's `BP` does not land on the §B.10.7 segment
/// boundary. The committed gray fixture was encoded *without*
/// predictable termination, so flipping the signalling bit makes the
/// signalled contract and the actual segment bytes disagree, and the
/// decode-time check surfaces it instead of returning a corrupt image.
#[test]
fn gray_53_predictable_termination_mismatch_is_rejected() {
    // Sanity: the unmodified fixture decodes pixel-exact.
    assert_eq!(
        decode_j2k(GRAY_53).expect("baseline").components[0].samples,
        gray_17x13_pattern()
    );
    let mutated = set_predictable_termination(GRAY_53);
    assert!(
        decode_j2k(&mutated).is_err(),
        "a stream falsely signalling predictable termination must be rejected"
    );
}

#[test]
fn truncated_codestream_is_rejected() {
    let cut = &GRAY_53[..GRAY_53.len() / 2];
    assert!(decode_j2k(cut).is_err());
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
