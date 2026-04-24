//! Round-8 black-box probes for the HH-sub-band OpenJPEG interop
//! investigation. The tests sweep `opj_compress` over carefully-chosen
//! input patterns and compare the extracted sub-bands against our
//! in-tree FDWT. Round 8 established that:
//!
//! 1. Single-pixel spikes at every `(x, y)` in a 16×16 canvas match
//!    bit-exactly.
//! 2. Smooth gradients, horizontal stripes, and vertical stripes match
//!    bit-exactly.
//! 3. Sparse two-spike patterns also match.
//! 4. Patterns with simultaneous high-frequency content on BOTH axes
//!    (checkerboard, the `opj16.pgm` testsrc texture) diverged on HH.
//!
//! Round 9 located the HH drift: `ctxno_zc` pre-clamped `ΣD` at 2 for
//! every orientation, collapsing Table D.1's HH column (labels 6 and 8
//! both cover `ΣD≥2`, but 8 specifically requires `ΣD≥3`). Un-clamping
//! `ΣD` on the HH path restores interop — the checker and other bi-
//! axial high-frequency patterns now round-trip bit-exact.
//!
//! The tests skip gracefully if `opj_compress` is missing on PATH.

use oxideav_jpeg2000::decode::tile::decode_subbands_round6;
use oxideav_jpeg2000::encode::dwt::fdwt_53;
use std::io::Write;
use std::process::Command;

/// Returns `true` if `opj_compress` is on PATH. Tests that need it skip
/// silently otherwise — useful for sandboxed CI.
fn opj_available() -> bool {
    Command::new("opj_compress")
        .arg("-h")
        .output()
        .map(|o| o.status.success() || !o.stdout.is_empty() || !o.stderr.is_empty())
        .unwrap_or(false)
}

fn write_pgm(path: &str, w: usize, h: usize, pixels: &[u8]) {
    let mut f = std::fs::File::create(path).unwrap();
    writeln!(f, "P5").unwrap();
    writeln!(f, "{w} {h}").unwrap();
    writeln!(f, "255").unwrap();
    f.write_all(pixels).unwrap();
}

fn opj_encode_1level(pgm: &str, j2k: &str) -> Result<(), String> {
    let out = Command::new("opj_compress")
        .args(["-i", pgm, "-o", j2k, "-n", "2"])
        .output()
        .map_err(|e| format!("spawn: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "opj_compress failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

/// `(ll, hl, lh, hh)` each 8x8 for a 16x16 input. Already `/ 2` from
/// our decoder's tier-1 magnitude convention.
fn extract_subbands(j2k_path: &str) -> (Vec<i32>, Vec<i32>, Vec<i32>, Vec<i32>) {
    let data = std::fs::read(j2k_path).unwrap();
    decode_subbands_round6(&data).unwrap()
}

fn our_fdwt_16(pixels: &[u8]) -> (Vec<i32>, Vec<i32>, Vec<i32>, Vec<i32>) {
    let mut canvas: Vec<i32> = pixels.iter().map(|&p| p as i32 - 128).collect();
    fdwt_53(&mut canvas, 16, 16, 16);
    let mut ll = vec![0i32; 64];
    let mut hl = vec![0i32; 64];
    let mut lh = vec![0i32; 64];
    let mut hh = vec![0i32; 64];
    for y in 0..8 {
        for x in 0..8 {
            ll[y * 8 + x] = canvas[y * 16 + x];
            hl[y * 8 + x] = canvas[y * 16 + (8 + x)];
            lh[y * 8 + x] = canvas[(8 + y) * 16 + x];
            hh[y * 8 + x] = canvas[(8 + y) * 16 + (8 + x)];
        }
    }
    (ll, hl, lh, hh)
}

fn hh_diff_count(ours: &[i32], opj: &[i32]) -> usize {
    ours.iter().zip(opj.iter()).filter(|(a, b)| a != b).count()
}

fn tmp_path(tag: &str) -> (String, String) {
    let dir = std::env::temp_dir();
    (
        dir.join(format!("oxideav_j2k_{tag}.pgm"))
            .to_string_lossy()
            .into_owned(),
        dir.join(format!("oxideav_j2k_{tag}.j2k"))
            .to_string_lossy()
            .into_owned(),
    )
}

/// Black-box probe: single-pixel spikes at various positions all
/// round-trip through `opj_compress` → our sub-band extractor bit-
/// exactly. Documents round-8's positive finding; gated on
/// `opj_compress` being on PATH.
#[test]
fn round8_spike_hh_bit_exact() {
    if !opj_available() {
        eprintln!("SKIP: opj_compress not on PATH");
        return;
    }
    let mut failures = Vec::new();
    for (sy, sx) in [
        (0, 0),
        (0, 1),
        (1, 0),
        (1, 1),
        (7, 0),
        (0, 7),
        (7, 7),
        (8, 8),
        (14, 14),
        (15, 15),
        (3, 5),
        (5, 3),
    ] {
        let mut px = vec![0u8; 256];
        px[sy * 16 + sx] = 255;
        let (pgm, j2k) = tmp_path(&format!("spike_{sy}_{sx}"));
        write_pgm(&pgm, 16, 16, &px);
        if let Err(e) = opj_encode_1level(&pgm, &j2k) {
            eprintln!("SKIP spike ({sy},{sx}): {e}");
            continue;
        }
        let (_, _, _, hh_opj) = extract_subbands(&j2k);
        let (_, _, _, hh_mine) = our_fdwt_16(&px);
        let diffs = hh_diff_count(&hh_mine, &hh_opj);
        if diffs != 0 {
            failures.push((sy, sx, diffs));
        }
    }
    assert!(
        failures.is_empty(),
        "single-pixel spikes should all match OPJ bit-exactly in HH; failures: {failures:?}"
    );
}

/// 16×16 checkerboard round-trip. Previously failed on 64/64 HH
/// samples; after the round-9 HH ZC-context fix it is 0/64 bit-exact
/// against `opj_compress`. Gated on `opj_compress` being on PATH.
#[test]
fn round8_checker_hh_divergence() {
    if !opj_available() {
        eprintln!("SKIP: opj_compress not on PATH");
        return;
    }
    let mut px = vec![0u8; 256];
    for y in 0..16 {
        for x in 0..16 {
            px[y * 16 + x] = if (x + y) & 1 == 0 { 0 } else { 255 };
        }
    }
    let (pgm, j2k) = tmp_path("checker16");
    write_pgm(&pgm, 16, 16, &px);
    if let Err(e) = opj_encode_1level(&pgm, &j2k) {
        eprintln!("SKIP: {e}");
        return;
    }
    let (ll_opj, hl_opj, lh_opj, hh_opj) = extract_subbands(&j2k);
    let (ll_mine, hl_mine, lh_mine, hh_mine) = our_fdwt_16(&px);
    assert_eq!(
        hh_diff_count(&ll_mine, &ll_opj),
        0,
        "LL must match bit-exactly on checker"
    );
    assert_eq!(
        hh_diff_count(&hl_mine, &hl_opj),
        0,
        "HL must match bit-exactly on checker"
    );
    assert_eq!(
        hh_diff_count(&lh_mine, &lh_opj),
        0,
        "LH must match bit-exactly on checker"
    );
    let hh_diffs = hh_diff_count(&hh_mine, &hh_opj);
    eprintln!("HH diffs on checker16: {hh_diffs}/64");
    eprintln!("ours (expected uniform -510):");
    for y in 0..8 {
        eprint!("  ");
        for x in 0..8 {
            eprint!("{:5} ", hh_mine[y * 8 + x]);
        }
        eprintln!();
    }
    eprintln!("OPJ (expected varied -303..-511):");
    for y in 0..8 {
        eprint!("  ");
        for x in 0..8 {
            eprint!("{:5} ", hh_opj[y * 8 + x]);
        }
        eprintln!();
    }
    // Round-9 witness: the HH drift was the erroneous `d.min(2)` clamp
    // on the zero-coding context for HH (T.800 Table D.1 distinguishes
    // ΣD=2 from ΣD≥3, so clamping ΣD at 2 collapsed labels 6/7/8 and
    // desynchronised the MQ coder on any block crossing a 3- or 4-
    // diagonal-neighbour coefficient). After fixing both `decode::t1`
    // and `encode::t1` the 16x16 checkerboard HH is now 0/64 bit-exact
    // against `opj_compress`.
    assert_eq!(
        hh_diffs, 0,
        "round-9 fix: checker HH must match OpenJPEG bit-exactly",
    );
}
