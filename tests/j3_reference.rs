//! Round-9 reference test: compare our FDWT against the normative J.3
//! example from T.800 Annex J.4.2 (5/3 reversible). Source samples from
//! Table J.3, expected 1HL/1LH/1HH from Tables J.15/J.16/J.17 and 2LL
//! from Table J.11.
//!
//! This is the most direct way to test the FDWT against the spec — no
//! third-party sources needed.

#![allow(clippy::unnecessary_cast, clippy::needless_range_loop)]

use oxideav_jpeg2000::encode::dwt::fdwt_53;

/// Table J.3 — 17 rows × 13 columns.
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

/// Table J.15 — 1HL sub-band (9 rows × 6 cols). For 13x17 input → LH+HL
/// each have size (ceil(13/2), ceil(17/2)) = (7, 9) or (6, 8) depending
/// on sub-band. For 1HL: u_b ∈ [0, floor(13/2)=6], v_b ∈ [0, ceil(17/2)=9).
/// Table width = 6, height = 9. Good.
#[rustfmt::skip]
const J3_1HL: [[i32; 6]; 9] = [
    [0, 0, 0, 0, 0, 0],   // v=0
    [0, 0, 0, 0, 0, 0],   // v=1
    [0, 1, 0, 1, 0, 0],   // v=2
    [0, 0, 0, 0,-1, 1],   // v=3
    [0, 0, 0, 0, 1, 1],   // v=4
    [0, 0, 1, 1, 0,-1],   // v=5
    [0, 0, 1, 0, 1, 1],   // v=6
    [0, 0, 0, 0, 0, 0],   // v=7
    [0, 0, 0, 0, 0, 0],   // v=8
];

/// Table J.16 — 1LH (8 rows × 7 cols).
#[rustfmt::skip]
const J3_1LH: [[i32; 7]; 8] = [
    [0, 0, 0, 0, 0, 0, 0],
    [0, 0, 1, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 1, 1],
    [0, 0, 1, 0, 0, 1, 1],
    [0, 0, 0, 0, 1, 0, 2],
    [0, 0, 0, 0, 0, 0, 1],
    [0, 0, 0, 0, 0, 0, 1],
    [0, 0, 0, 0, 1, 1, 0],
];

/// Table J.17 — 1HH (8 rows × 6 cols).
#[rustfmt::skip]
const J3_1HH: [[i32; 6]; 8] = [
    [0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 0],
    [0, 0, 1, 0, 1, 0],
    [0, 0, 0, 0, 0, 1],
    [0, 0, 0, 0, 0, 1],
    [0, 0, 0, 1, 0, 0],
    [0, 0, 0, 0, 0, 1],
    [0, 0, 0, 0,-1, 0],
];

/// Reference-correct 1-D forward 5/3 lift per T.800 F-9/F-10. Uses
/// explicit PSE_O mirror indexing. Output is deinterleaved: `[L0..Lm, H0..Hk]`.
fn ref_fdwt_53_1d(x: &[i32]) -> Vec<i32> {
    let n = x.len();
    if n < 2 {
        return x.to_vec();
    }
    // Mirror-extend the input via PSE_O(i, 0, n) for i in -2..n+2.
    let pse = |i: i64| -> i32 {
        let n_i = n as i64;
        let m = 2 * (n_i - 1);
        let r = i.rem_euclid(m);
        let p = r.min(m - r);
        x[p as usize]
    };
    // Compute Y(2k+1) for k=-1..ceil(n/2)
    let m_hi = (n as i64 + 1) / 2;
    let mut y_odd = std::collections::HashMap::new(); // index → Y value
    for k in -1..=m_hi {
        let idx = 2 * k + 1;
        let y = pse(idx) - ((pse(idx - 1) + pse(idx + 1)).div_euclid(2) as i32);
        y_odd.insert(idx, y);
    }
    // Compute Y(2k) for k=0..ceil(n/2)
    let mut y_even = std::collections::HashMap::new();
    for k in 0..m_hi {
        let idx = 2 * k;
        let y_m1 = *y_odd.get(&(idx - 1)).unwrap();
        let y_p1 = *y_odd.get(&(idx + 1)).unwrap();
        let y = pse(idx) + ((y_m1 + y_p1 + 2).div_euclid(4) as i32);
        y_even.insert(idx, y);
    }
    // Deinterleave into output.
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

/// Reference 2-D FDWT using explicit PSE mirror on each axis. Column
/// pass first, then row pass. After both, the layout is deinterleaved.
fn ref_fdwt_53_2d(src: &[i32], w: usize, h: usize) -> Vec<i32> {
    let mut buf = src.to_vec();
    // Column pass.
    for x in 0..w {
        let col: Vec<i32> = (0..h).map(|y| buf[y * w + x]).collect();
        let out = ref_fdwt_53_1d(&col);
        for y in 0..h {
            buf[y * w + x] = out[y];
        }
    }
    // Row pass.
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
#[ignore = "round-9: our 2D FDWT vs reference 2D FDWT on J.3"]
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
    let mut diff = 0;
    for y in 0..h {
        for x in 0..w {
            if ours[y * w + x] != ref_out[y * w + x] {
                diff += 1;
            }
        }
    }
    eprintln!("2D-ours vs 2D-ref diffs: {diff}/{}", w * h);
    eprintln!("Ours:");
    for y in 0..h {
        eprint!("  ");
        for x in 0..w {
            eprint!("{:4} ", ours[y * w + x]);
        }
        eprintln!();
    }
    eprintln!("Ref:");
    for y in 0..h {
        eprint!("  ");
        for x in 0..w {
            eprint!("{:4} ", ref_out[y * w + x]);
        }
        eprintln!();
    }
}

#[test]
#[ignore = "round-9 reference on column 9 of J.3"]
fn j3_col9_1d_matches_ref() {
    let col9: Vec<i32> = (0..17).map(|r| J3_SRC[r][9]).collect();
    let expected = ref_fdwt_53_1d(&col9);
    let mut ours = col9.clone();
    oxideav_jpeg2000::encode::dwt::fdwt_53_1d(&mut ours);
    eprintln!("col9 input : {col9:?}");
    eprintln!("col9 ref   : {expected:?}");
    eprintln!("col9 ours  : {ours:?}");
    assert_eq!(
        ours, expected,
        "1-D forward 5/3 diverges from PSE-ref on J.3 col 9"
    );
}

/// Round-9 critical regression probe: compare our FDWT output vs OPJ's
/// extracted sub-bands on the opj16 checker-ish fixture. If our FDWT
/// matches OPJ bit-exactly on all four sub-bands, the FDWT is not the
/// source of the HH drift — look elsewhere (tier-1 magnitude, bpno).
#[test]
#[ignore = "round-9: our FDWT vs OPJ on opj16 pattern"]
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
    eprintln!("opj16 LL diffs: {}/64", cnt(&ll_ours, &ll_opj));
    eprintln!("opj16 HL diffs: {}/64", cnt(&hl_ours, &hl_opj));
    eprintln!("opj16 LH diffs: {}/64", cnt(&lh_ours, &lh_opj));
    eprintln!("opj16 HH diffs: {}/64", cnt(&hh_ours, &hh_opj));
}

/// Compare our 1-level FDWT against OPJ's decoded sub-bands on J.3.
#[test]
#[ignore = "round-9: compare our FDWT against opj_compress on J.3"]
fn j3_vs_opj_1level() {
    let j2k_1 = std::fs::read("tests/fixtures/j3.j2k").unwrap();
    let (ll_opj, hl_opj, lh_opj, hh_opj) =
        oxideav_jpeg2000::decode::tile::decode_subbands_round6(&j2k_1).unwrap();
    // OPJ's decoded sub-bands for 13x17 input (1-level):
    //   LL: 7x9, HL: 6x9, LH: 7x8, HH: 6x8.
    eprintln!("opj LL ({}):", ll_opj.len());
    for y in 0..9 {
        eprint!("  ");
        for x in 0..7 {
            eprint!("{:4} ", ll_opj[y * 7 + x]);
        }
        eprintln!();
    }
    eprintln!("opj HL ({}):", hl_opj.len());
    for y in 0..9 {
        eprint!("  ");
        for x in 0..6 {
            eprint!("{:4} ", hl_opj[y * 6 + x]);
        }
        eprintln!();
    }
    eprintln!("opj LH ({}):", lh_opj.len());
    for y in 0..8 {
        eprint!("  ");
        for x in 0..7 {
            eprint!("{:4} ", lh_opj[y * 7 + x]);
        }
        eprintln!();
    }
    eprintln!("opj HH ({}):", hh_opj.len());
    for y in 0..8 {
        eprint!("  ");
        for x in 0..6 {
            eprint!("{:4} ", hh_opj[y * 6 + x]);
        }
        eprintln!();
    }
    // Now compute our FDWT on the (DC-shifted) J.3 source.
    let w = 13usize;
    let h = 17usize;
    let mut canvas: Vec<i32> = Vec::with_capacity(w * h);
    for row in &J3_SRC {
        for &v in row {
            canvas.push(v);
        }
    }
    fdwt_53(&mut canvas, w, h, w);
    eprintln!("ours HL (6x9 at canvas cols 7..13, rows 0..9):");
    for y in 0..9 {
        eprint!("  ");
        for x in 0..6 {
            eprint!("{:4} ", canvas[y * w + (7 + x)]);
        }
        eprintln!();
    }
    let mut hl_match = 0;
    let mut hl_mismatch = 0;
    for y in 0..9 {
        for x in 0..6 {
            if canvas[y * w + (7 + x)] == hl_opj[y * 6 + x] {
                hl_match += 1;
            } else {
                hl_mismatch += 1;
            }
        }
    }
    eprintln!("HL match: {hl_match}, mismatch: {hl_mismatch}");

    // Now check LH and HH too.
    let mut lh_mismatch = 0;
    for y in 0..8 {
        for x in 0..7 {
            let ours_val = canvas[(9 + y) * w + x];
            let opj_val = lh_opj[y * 7 + x];
            if ours_val != opj_val {
                lh_mismatch += 1;
                eprintln!("  LH mismatch at (x={x}, y={y}): ours={ours_val} opj={opj_val}");
            }
        }
    }
    eprintln!("LH mismatches: {lh_mismatch}/56");

    let mut hh_mismatch = 0;
    for y in 0..8 {
        for x in 0..6 {
            let ours_val = canvas[(9 + y) * w + (7 + x)];
            let opj_val = hh_opj[y * 6 + x];
            if ours_val != opj_val {
                hh_mismatch += 1;
                eprintln!("  HH mismatch at (x={x}, y={y}): ours={ours_val} opj={opj_val}");
            }
        }
    }
    eprintln!("HH mismatches: {hh_mismatch}/48");
    let mut ll_mismatch = 0;
    for y in 0..9 {
        for x in 0..7 {
            let ours_val = canvas[y * w + x];
            let opj_val = ll_opj[y * 7 + x];
            if ours_val != opj_val {
                ll_mismatch += 1;
                eprintln!("  LL mismatch at (x={x}, y={y}): ours={ours_val} opj={opj_val}");
            }
        }
    }
    eprintln!("LL mismatches: {ll_mismatch}/63");
}

#[test]
#[ignore = "round-9 reference against T.800 Annex J.4.2"]
fn j3_fdwt_matches_spec_reference() {
    let w = 13usize;
    let h = 17usize;
    let mut canvas: Vec<i32> = Vec::with_capacity(w * h);
    for row in &J3_SRC {
        for &v in row {
            canvas.push(v);
        }
    }
    // Apply a single level of 5/3 FDWT (since J.15/J.16/J.17 are 1HL/1LH/1HH,
    // the first level decomposition).
    fdwt_53(&mut canvas, w, h, w);
    // Sub-band sizes after one level:
    //   LL size: (ceil(13/2), ceil(17/2)) = (7, 9)
    //   HL size: (floor(13/2), ceil(17/2)) = (6, 9)
    //   LH size: (ceil(13/2), floor(17/2)) = (7, 8)
    //   HH size: (floor(13/2), floor(17/2)) = (6, 8)
    //
    // Our layout: after FDWT deinterleave, L-cols at [0..ceil(w/2)=7], H-cols at [7..13].
    // L-rows at [0..ceil(h/2)=9], H-rows at [9..17].
    // So:
    //   LL: (x, y) for x in 0..7, y in 0..9
    //   HL: (x, y) for x in 7..13, y in 0..9   (width 6)
    //   LH: (x, y) for x in 0..7, y in 9..17   (width 7, height 8)
    //   HH: (x, y) for x in 7..13, y in 9..17  (width 6, height 8)

    // Check 1HL (width 6, height 9).
    eprintln!("1HL (ours):");
    let mut fail_hl = 0;
    for y in 0..9 {
        eprint!("  ");
        for x in 0..6 {
            let ours = canvas[y * w + (7 + x)];
            let spec = J3_1HL[y][x];
            eprint!("{ours:3}({spec:2}) ");
            if ours != spec {
                fail_hl += 1;
            }
        }
        eprintln!();
    }
    eprintln!("HL mismatches: {fail_hl}/54");

    // Check 1LH (width 7, height 8).
    eprintln!("1LH (ours):");
    let mut fail_lh = 0;
    for y in 0..8 {
        eprint!("  ");
        for x in 0..7 {
            let ours = canvas[(9 + y) * w + x];
            let spec = J3_1LH[y][x];
            eprint!("{ours:3}({spec:2}) ");
            if ours != spec {
                fail_lh += 1;
            }
        }
        eprintln!();
    }
    eprintln!("LH mismatches: {fail_lh}/56");

    // Check 1HH (width 6, height 8).
    eprintln!("1HH (ours):");
    let mut fail_hh = 0;
    for y in 0..8 {
        eprint!("  ");
        for x in 0..6 {
            let ours = canvas[(9 + y) * w + (7 + x)];
            let spec = J3_1HH[y][x];
            eprint!("{ours:3}({spec:2}) ");
            if ours != spec {
                fail_hh += 1;
            }
        }
        eprintln!();
    }
    eprintln!("HH mismatches: {fail_hh}/48");

    assert_eq!(fail_hl, 0, "HL must match spec J.15");
    assert_eq!(fail_lh, 0, "LH must match spec J.16");
    assert_eq!(fail_hh, 0, "HH must match spec J.17");
}
