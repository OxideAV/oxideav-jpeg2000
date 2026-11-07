//! Image-grid origins at and above 2^31 (T.800 §A.5.1, Table A.9: `XOsiz`,
//! `YOsiz`, `XTOsiz`, `YTOsiz` and `Xsiz`/`Ysiz` span `0 ..= 2^32 − 1`).
//!
//! The encoder anchors the reference grid at `(0, 0)`. Translating every
//! grid coordinate of a single-tile codestream by a multiple of
//! `2^(15 + NL + 1)` leaves the whole §B.5 – §B.7 partition (resolution
//! corners, Equation B-15 band corners, precinct and code-block cells) and
//! the §F.3.6 lifting parity unchanged, so the decoded planes must be
//! identical to the untranslated stream's.

use oxideav_jpeg2000::encode::encode_j2k;
use oxideav_jpeg2000::{EncodeOptions, Error, decode_j2k};

fn noise(w: u32, h: u32, seed: u32) -> Vec<u8> {
    let mut s = seed;
    (0..w * h)
        .map(|_| {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (s >> 16) as u8
        })
        .collect()
}

/// Rewrite the SIZ marker (it always follows SOC) to translate the image
/// and tile grid by `(sx, sy)`. Single-tile streams only.
fn translate_siz(stream: &[u8], sx: u32, sy: u32) -> Vec<u8> {
    assert_eq!(&stream[0..4], &[0xFF, 0x4F, 0xFF, 0x51]);
    let mut out = stream.to_vec();
    let field = |out: &[u8], i: usize| -> u32 {
        let p = 8 + 4 * i;
        u32::from_be_bytes([out[p], out[p + 1], out[p + 2], out[p + 3]])
    };
    let put = |out: &mut [u8], i: usize, v: u32| {
        let p = 8 + 4 * i;
        out[p..p + 4].copy_from_slice(&v.to_be_bytes());
    };
    // Xsiz Ysiz XOsiz YOsiz XTsiz YTsiz XTOsiz YTOsiz
    let (xsiz, ysiz) = (field(&out, 0), field(&out, 1));
    let (xo, yo) = (field(&out, 2), field(&out, 3));
    let (xto, yto) = (field(&out, 6), field(&out, 7));
    assert_eq!((xo, yo, xto, yto), (0, 0, 0, 0));
    put(&mut out, 0, xsiz.checked_add(sx).unwrap());
    put(&mut out, 1, ysiz.checked_add(sy).unwrap());
    put(&mut out, 2, sx);
    put(&mut out, 3, sy);
    put(&mut out, 6, sx);
    put(&mut out, 7, sy);
    out
}

/// Encode `w × h` noise with `opts`, then decode the stream as written and
/// translated by every `(sx, sy)` in `shifts`; all decodes must agree.
fn assert_translation_invariant(w: u32, h: u32, opts: &EncodeOptions, shifts: &[(u32, u32)]) {
    let planes = [noise(w, h, 7), noise(w, h, 99), noise(w, h, 1234)];
    let refs: Vec<&[u8]> = planes.iter().map(Vec::as_slice).collect();
    let stream = encode_j2k(&refs, w, h, opts).unwrap();
    let base = decode_j2k(&stream).unwrap();
    for &(sx, sy) in shifts {
        let moved = translate_siz(&stream, sx, sy);
        let got = decode_j2k(&moved).unwrap_or_else(|e| panic!("shift ({sx:#x}, {sy:#x}): {e:?}"));
        assert_eq!((got.width, got.height), (w, h));
        assert_eq!(got.components.len(), base.components.len());
        for (c, (a, b)) in base.components.iter().zip(&got.components).enumerate() {
            assert_eq!(
                (a.width, a.height),
                (b.width, b.height),
                "shift ({sx:#x}, {sy:#x}) component {c}"
            );
            assert_eq!(
                a.samples, b.samples,
                "shift ({sx:#x}, {sy:#x}) component {c}"
            );
        }
    }
}

fn kernels(opts: EncodeOptions) -> [EncodeOptions; 2] {
    [opts.clone().with_lossless(), opts.with_lossy(8)]
}

/// Default (maximum, `PP = 15`) precincts: shifts that are multiples of
/// `2^(15 + NL)` keep every partition, from `2^31` up to the top of the
/// Table A.9 range.
#[test]
fn origins_at_and_above_2_pow_31_decode_like_the_origin() {
    let (w, h) = (13u32, 11u32);
    let shifts = [
        (0x8000_0000, 0x8000_0000),
        (0x8000_0000, 0),
        (0, 0x8000_0000),
        (0xFFF0_0000, 0xFFFC_0000),
    ];
    for levels in [0u8, 1, 3] {
        for opts in kernels(
            EncodeOptions::default()
                .with_decomposition_levels(levels)
                .with_tile_size((w, h)),
        ) {
            assert_translation_invariant(w, h, &opts, &shifts);
        }
    }
}

/// `NL = 0`, one 64 × 64 code-block and one `2^15` precinct that both end
/// exactly at `2^32`: the §B.6 / §B.7 cell edges overflow `u32`.
/// Regression: a carried-out cell edge used to wrap to `0`, emptying the
/// precinct, so every sample decoded as the DC level.
#[test]
fn cell_edges_at_2_pow_32_keep_their_code_blocks() {
    let (w, h) = (13u32, 11u32);
    let shifts = [
        (0xFFFF_FFC0, 0),
        (0, 0xFFFF_FFC0),
        (0xFFFF_FFC0, 0xFFFF_FFC0),
        (0xFFFF_8000, 0xFFFF_FF00),
    ];
    for opts in kernels(
        EncodeOptions::default()
            .with_decomposition_levels(0)
            .with_tile_size((w, h)),
    ) {
        assert_translation_invariant(w, h, &opts, &shifts);
    }
}

/// 4 × 4 code-blocks and `PP = 2` precincts make a shift of `16` keep the
/// partition at `NL = 2`, so the grid can end at `2^32 − 1`. The
/// resolution-level-1 origin is then `0x7FFF_FFF8` with 8 samples: its
/// `i32` end `0x8000_0000` overflowed in the inverse DWT (a debug-build
/// panic, a wrapped index range in release).
#[test]
fn level_origin_next_to_i32_max_reconstructs() {
    let (w, h) = (15u32, 15u32);
    let shifts = [
        (0xFFFF_FFF0, 0xFFFF_FFF0),
        (0xFFFF_FFF0, 0),
        (0x7FFF_FFF0, 0xFFFF_FFF0),
    ];
    for levels in [1u8, 2] {
        let base = EncodeOptions::default()
            .with_decomposition_levels(levels)
            .with_code_block_exp((2, 2))
            .with_precincts(vec![0x22; levels as usize + 1])
            .with_tile_size((w, h));
        for opts in kernels(base) {
            assert_translation_invariant(w, h, &opts, &shifts);
        }
    }
}

/// Overwrite SIZ fields `(index, value)` (index 0 = `Xsiz` … 7 = `YTOsiz`).
fn patch_siz(stream: &[u8], fields: &[(usize, u32)]) -> Vec<u8> {
    let mut out = stream.to_vec();
    for &(i, v) in fields {
        let p = 8 + 4 * i;
        out[p..p + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

/// A single tile with `XTsiz = Xsiz` anchored at `XTOsiz = XOsiz = 2^31`
/// satisfies Equation B-4 although `XTsiz + XTOsiz` passes `u32`; it used
/// to be rejected as malformed.
#[test]
fn whole_grid_tile_above_2_pow_31_is_accepted() {
    let (w, h) = (13u32, 11u32);
    let planes = [noise(w, h, 5)];
    let refs: Vec<&[u8]> = planes.iter().map(Vec::as_slice).collect();
    let stream = encode_j2k(&refs, w, h, &EncodeOptions::default().with_lossless()).unwrap();
    let base = decode_j2k(&stream).unwrap();
    let s = 0x8000_0000u32;
    let moved = patch_siz(
        &stream,
        &[
            (0, s + w),
            (1, s + h),
            (2, s),
            (3, s),
            (4, s + w),
            (5, s + h),
            (6, s),
            (7, s),
        ],
    );
    let got = decode_j2k(&moved).unwrap();
    assert_eq!(got.components[0].samples, base.components[0].samples);
}

/// Grids whose single component plane cannot be addressed are a limit
/// error, never an allocation panic.
#[test]
fn unaddressable_planes_are_errors() {
    let planes = [noise(13, 11, 5)];
    let refs: Vec<&[u8]> = planes.iter().map(Vec::as_slice).collect();
    let stream = encode_j2k(&refs, 13, 11, &EncodeOptions::default()).unwrap();
    for (origin, tile) in [(0u32, u32::MAX), (0x8000_0000, u32::MAX), (1, 0x7FFF_FFFF)] {
        let bad = patch_siz(
            &stream,
            &[
                (0, u32::MAX),
                (1, u32::MAX),
                (2, origin),
                (3, origin),
                (4, tile),
                (5, tile),
                (6, origin.min(1)),
                (7, origin.min(1)),
            ],
        );
        match decode_j2k(&bad) {
            Err(Error::LimitExceeded(_)) => {}
            other => panic!(
                "origin {origin:#x}: {:?}",
                other.map(|d| (d.width, d.height))
            ),
        }
    }
}
