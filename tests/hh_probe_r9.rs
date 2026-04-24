//! Round-9 HH-interop regression probe.
//!
//! Before round 9 this fixture lit up 50/64 HH-coefficient mismatches
//! against the OpenJPEG-emitted `opj16_l1.j2k` codestream: our forward
//! 5/3 produced the spec-correct sub-band values for LL/HL/LH but the
//! HH-band MQ context lookup inside the encoder/decoder was clamping
//! ΣD at 2 for every orientation, which collapses Table D.1's HH
//! column (context 6 for ΣD=2, context 8 for ΣD≥3) and desynchronises
//! every arithmetic-coded HH bit after the first diagonal cluster.
//!
//! The `ctxno_zc(..., Orient::Hh)` path in both `src/encode/t1.rs` and
//! `src/decode/t1.rs` no longer pre-clamps ΣD; this test pins the fix
//! by asserting 0/64 HH drift at the `decode_subbands_round6` level.

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

#[test]
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
    assert_eq!(
        diff_count, 0,
        "HH must be bit-exact against OpenJPEG (pre-round-9: 50/64 drift)"
    );
}
