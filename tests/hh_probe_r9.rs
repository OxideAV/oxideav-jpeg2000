//! Round-9 probe: verify our FDWT's HH output matches OPJ's HH output
//! for the opj16 fixture. Before round 9 this reported 50/64 HH diffs;
//! after the ZC-HH context fix (removing the erroneous `d.min(2)` clamp
//! for the HH orientation) the HH is 0/64 bit-exact against OpenJPEG.
//!
//! Run with:
//! ```bash
//! cargo test --test hh_probe_r9 -- --ignored --nocapture
//! ```

use oxideav_jpeg2000::decode::tile::decode_subbands_round6;
use oxideav_jpeg2000::encode::dwt::fdwt_53;

const OPJ16_J2K: &[u8] = include_bytes!("fixtures/opj16_l1.j2k");
const OPJ16_PGM: &[u8] = include_bytes!("fixtures/opj16.pgm");

fn parse_pgm(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    assert_eq!(&bytes[0..2], b"P5");
    let mut i = 2usize;
    let mut toks: Vec<String> = Vec::new();
    while toks.len() < 3 {
        while i < bytes.len() && (bytes[i] == b'\n' || bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'#' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;
        while i < bytes.len()
            && bytes[i] != b'\n'
            && bytes[i] != b' '
            && bytes[i] != b'\t'
            && bytes[i] != b'#'
        {
            i += 1;
        }
        toks.push(String::from_utf8_lossy(&bytes[start..i]).into_owned());
    }
    i += 1;
    let w: u32 = toks[0].parse().unwrap();
    let h: u32 = toks[1].parse().unwrap();
    (w, h, bytes[i..].to_vec())
}

/// Forward-DWT the opj16 PGM and compare the resulting HH sub-band to
/// what OpenJPEG's encoded stream decodes to. Before round 9 this
/// reported 50/64 diffs on the 8x8 HH block; afterwards, 0/64.
#[test]
#[ignore = "round-9 HH interop probe"]
fn r9_hh_compare() {
    let (_ll, _hl, _lh, hh_opj) = decode_subbands_round6(OPJ16_J2K).unwrap();
    let (w, h, raw) = parse_pgm(OPJ16_PGM);
    assert_eq!((w, h), (16, 16));
    let mut canvas: Vec<i32> = raw.iter().map(|&b| b as i32 - 128).collect();
    fdwt_53(&mut canvas, 16, 16, 16);
    let mut hh_ours = vec![0i32; 64];
    for y in 0..8 {
        for x in 0..8 {
            hh_ours[y * 8 + x] = canvas[(8 + y) * 16 + (8 + x)];
        }
    }
    let mut diff_count = 0;
    for i in 0..64 {
        if hh_ours[i] != hh_opj[i] {
            diff_count += 1;
        }
    }
    eprintln!("HH FDWT vs OPJ: {diff_count}/64 mismatches");
    assert_eq!(diff_count, 0, "HH must be bit-exact against OpenJPEG");
}
