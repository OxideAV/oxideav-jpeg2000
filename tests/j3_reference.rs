//! Round-9 FDWT-vs-OpenJPEG regression tests.
//!
//! Exercises the forward 5/3 transform in three settings:
//!
//! 1. `j3_col9_1d_matches_ref` — spec-correct 1-D reference (explicit
//!    `PSE_O` mirror) against `fdwt_53_1d` on column 9 of Annex J
//!    Table J.3.
//! 2. `j3_2d_ours_vs_ref_2d` — spec-correct 2-D reference against
//!    `fdwt_53` on the full J.3 tile-component.
//! 3. `j3_vs_opj_1level` / `opj16_fdwt_vs_opj` — bit-exact agreement
//!    with OpenJPEG's own output on the fixtures used by the interop
//!    tests.
//!
//! T.800 Annex J is informative, and the 5/3 example tables J.15/J.16/
//! J.17 disagree with the normative §F.2 lifting for a handful of
//! cells — we therefore compare against a purely-spec-correct reference
//! instead of asserting on those tables.

#![allow(clippy::unnecessary_cast, clippy::needless_range_loop)]

use oxideav_jpeg2000::encode::dwt::fdwt_53;

/// Table J.3 — 17 rows × 13 columns. Tile-component samples (already
/// DC-level-shifted).
#[rustfmt::skip]
const J3_SRC: [[i32; 13]; 17] = [
    [ 0, 1, 2, 3, 4, 5, 6, 7, 8, 9,10,11,12],
    [ 1, 1, 2, 3, 4, 5, 6, 7, 8, 9,10,11,12],
    [ 2, 2, 2, 3, 4, 5, 6, 7, 8, 9,10,11,12],
    [ 3, 3, 3, 3, 4, 5, 5, 6, 7, 8, 9,10,11],
    [ 4, 4, 4, 4, 4, 5, 6, 7, 8, 9,10,11,12],
    [ 5, 5, 5, 5, 5, 6, 7, 7, 8, 9,10,11,12],
    [ 6, 6, 6, 6, 6, 7, 7, 8, 9,10,10,11,12],
    [ 7, 7, 7, 7, 7, 8, 8, 9,10,11,12,13,13],
    [ 8, 8, 8, 8, 8, 8, 9,10,10,11,12,12,13],
    [ 9, 9, 9, 9, 9, 9,10,11,12,12,13,14,14],
    [10,10,10,10,10,11,11,12,12,13,14,14,15],
    [11,11,11,11,11,12,12,13,13,14,14,15,15],
    [12,12,12,12,12,13,13,13,14,15,15,16,16],
    [13,13,13,13,13,13,14,14,15,15,16,17,17],
    [14,14,14,14,14,15,15,16,16,16,17,17,18],
    [15,15,15,15,15,15,16,16,17,17,18,18,19],
    [16,16,16,16,16,16,16,17,17,17,18,18,20],
];

/// Reference-correct 1-D forward 5/3 lift per T.800 F-9/F-10. Uses
/// explicit `PSE_O` mirror indexing with the wider [-2, n+2] support
/// the spec's step ranges call for. Output is deinterleaved as
/// `[L0..Lm-1, H0..Hk-1]`.
fn ref_fdwt_53_1d(x: &[i32]) -> Vec<i32> {
    let n = x.len();
    if n < 2 {
        return x.to_vec();
    }
    let pse = |i: i64| -> i32 {
        let n_i = n as i64;
        let m = 2 * (n_i - 1);
        let r = i.rem_euclid(m);
        let p = r.min(m - r);
        x[p as usize]
    };
    let m_hi = (n as i64 + 1) / 2;
    let mut y_odd = std::collections::HashMap::new();
    for k in -1..=m_hi {
        let idx = 2 * k + 1;
        let y = pse(idx) - ((pse(idx - 1) + pse(idx + 1)).div_euclid(2) as i32);
        y_odd.insert(idx, y);
    }
    let mut y_even = std::collections::HashMap::new();
    for k in 0..m_hi {
        let idx = 2 * k;
        let y_m1 = *y_odd.get(&(idx - 1)).unwrap();
        let y_p1 = *y_odd.get(&(idx + 1)).unwrap();
        let y = pse(idx) + ((y_m1 + y_p1 + 2).div_euclid(4) as i32);
        y_even.insert(idx, y);
    }
    let m = n.div_ceil(2);
    let mut out = vec![0i32; n];
    for k in 0..m {
        out[k] = *y_even.get(&(2 * k as i64)).unwrap();
    }
    let k_high = n - m;
    for k in 0..k_high {
        out[m + k] = *y_odd.get(&(2 * k as i64 + 1)).unwrap();
    }
    out
}

/// Reference 2-D FDWT via the 1-D reference applied column-first then
/// row-first, in line with T.800 §F.4.2 (2D_SD: VER_SD → HOR_SD).
fn ref_fdwt_53_2d(src: &[i32], w: usize, h: usize) -> Vec<i32> {
    let mut buf = src.to_vec();
    for x in 0..w {
        let col: Vec<i32> = (0..h).map(|y| buf[y * w + x]).collect();
        let out = ref_fdwt_53_1d(&col);
        for y in 0..h {
            buf[y * w + x] = out[y];
        }
    }
    for y in 0..h {
        let row: Vec<i32> = (0..w).map(|x| buf[y * w + x]).collect();
        let out = ref_fdwt_53_1d(&row);
        for x in 0..w {
            buf[y * w + x] = out[x];
        }
    }
    buf
}

#[test]
fn j3_col9_1d_matches_ref() {
    let col9: Vec<i32> = (0..17).map(|r| J3_SRC[r][9]).collect();
    let expected = ref_fdwt_53_1d(&col9);
    let mut ours = col9.clone();
    oxideav_jpeg2000::encode::dwt::fdwt_53_1d(&mut ours);
    assert_eq!(
        ours, expected,
        "1-D forward 5/3 diverges from PSE-mirror reference on J.3 col 9"
    );
}

#[test]
fn j3_2d_ours_vs_ref_2d() {
    let w = 13usize;
    let h = 17usize;
    let mut src = Vec::with_capacity(w * h);
    for row in &J3_SRC {
        for &v in row {
            src.push(v);
        }
    }
    let ref_out = ref_fdwt_53_2d(&src, w, h);
    let mut ours = src.clone();
    fdwt_53(&mut ours, w, h, w);
    for i in 0..w * h {
        assert_eq!(
            ours[i],
            ref_out[i],
            "2-D FDWT diverges from PSE-mirror reference at index {i} (x={}, y={})",
            i % w,
            i / w
        );
    }
}

/// Our FDWT on the `opj16.pgm` fixture must match OpenJPEG's decoded
/// sub-bands bit-exactly on all four quadrants. This was the round-9
/// regression: HH diverged on 50/64 cells before the ZC-HH context fix.
#[test]
fn opj16_fdwt_vs_opj() {
    let j2k = std::fs::read("tests/fixtures/opj16_l1.j2k").unwrap();
    let (ll_opj, hl_opj, lh_opj, hh_opj) =
        oxideav_jpeg2000::decode::tile::decode_subbands_round6(&j2k).unwrap();
    let pgm = std::fs::read("tests/fixtures/opj16.pgm").unwrap();
    let mut i = 0;
    let mut nl = 0;
    while i < pgm.len() && nl < 3 {
        if pgm[i] == b'\n' {
            nl += 1;
        }
        i += 1;
    }
    let raw = &pgm[i..];
    let w = 16usize;
    let h = 16usize;
    let mut canvas: Vec<i32> = raw.iter().map(|&b| b as i32 - 128).collect();
    fdwt_53(&mut canvas, w, h, w);
    let (mut ll_ours, mut hl_ours, mut lh_ours, mut hh_ours) = (
        vec![0i32; 64],
        vec![0i32; 64],
        vec![0i32; 64],
        vec![0i32; 64],
    );
    for y in 0..8 {
        for x in 0..8 {
            ll_ours[y * 8 + x] = canvas[y * w + x];
            hl_ours[y * 8 + x] = canvas[y * w + (8 + x)];
            lh_ours[y * 8 + x] = canvas[(8 + y) * w + x];
            hh_ours[y * 8 + x] = canvas[(8 + y) * w + (8 + x)];
        }
    }
    let cnt = |a: &[i32], b: &[i32]| a.iter().zip(b.iter()).filter(|(x, y)| x != y).count();
    assert_eq!(cnt(&ll_ours, &ll_opj), 0, "opj16 LL must match OpenJPEG");
    assert_eq!(cnt(&hl_ours, &hl_opj), 0, "opj16 HL must match OpenJPEG");
    assert_eq!(cnt(&lh_ours, &lh_opj), 0, "opj16 LH must match OpenJPEG");
    assert_eq!(cnt(&hh_ours, &hh_opj), 0, "opj16 HH must match OpenJPEG");
}
