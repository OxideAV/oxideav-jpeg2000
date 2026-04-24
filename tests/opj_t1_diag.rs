//! **Diagnostic harness (gated behind `#[ignore]`).** Prints the
//! forward-5/3-DWT of the `opj16.pgm` reference next to our decoder's
//! reconstruction so the residual ±1 LSB drift versus the OpenJPEG
//! fixture is visible in one place.
//!
//! Run with:
//!
//! ```bash
//! cargo test --test opj_t1_diag -- --ignored --nocapture
//! ```
//!
//! Round 5 findings
//! ----------------
//!
//! - Our own forward-DWT of `opj16.pgm` matches what ffmpeg's
//!   spec-conformant decoder recovers from OpenJPEG's `opj16_l1.j2k`
//!   (both end at the same sub-band integers), so the coefficients that
//!   OpenJPEG encoded and the coefficients our encoder sees for the
//!   same source are identical.
//! - Our encoder's MQ byte stream and OpenJPEG's MQ byte stream agree
//!   byte-for-byte for the first 21 bytes of the LL code-block — which
//!   proves the MQ state machine and context-probability tables are in
//!   lockstep across all of bit-planes 8 through ~bpno 5.
//! - ffmpeg's decode of our `.j2k` output differs from the source PGM
//!   by ±1 LSB on scattered samples, so our encoder is producing a
//!   bit-consistent-with-itself but non-spec-conformant bit stream
//!   starting somewhere past byte 21 of the first code-block.
//! - Our decoder's direct tier-1 reconstruction of LL(1, 0) gives raw
//!   magnitude 157 (⇒ -78 after `/2`) when the correct raw is 159
//!   (⇒ -79). That is exactly one magref bit different at bpno=1,
//!   suggesting the last-magref-at-bpno=1 step is interpreting
//!   OpenJPEG's stream with one flipped bit. The same bias shows up
//!   symmetrically in our encoder output, which is why our own
//!   encode/decode round-trip stays bit-exact while the OPJ / ffmpeg
//!   interop fails.

use oxideav_jpeg2000::codestream;
use oxideav_jpeg2000::decode::tile::{
    build_subbands, decode_tile_with_params, parse_cod, parse_qcd, DecodeParams,
};
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

/// Runs our forward DWT on the PGM (after DC level shift) and prints
/// it next to our decoder's tier-1 output at the LL sub-band location.
#[test]
#[ignore = "diagnostic; prints sub-band drift between our decode and the OPJ fixture"]
fn diag_opj16_sub_band_values() {
    let (w, h, pgm) = parse_pgm(OPJ16_PGM);
    assert_eq!((w, h), (16, 16));

    // Forward-DWT the DC-shifted reference.
    let mut canvas: Vec<i32> = pgm.iter().map(|&b| b as i32 - 128).collect();
    fdwt_53(&mut canvas, w as usize, h as usize, w as usize);
    println!("forward-DWT of reference (level 1):");
    let hw = (w / 2) as usize;
    let hh = (h / 2) as usize;
    println!(" LL (8x8):");
    for y in 0..hh {
        for x in 0..hw {
            print!("{:5} ", canvas[y * w as usize + x]);
        }
        println!();
    }
    println!(" HL (8x8):");
    for y in 0..hh {
        for x in 0..hw {
            print!("{:5} ", canvas[y * w as usize + hw + x]);
        }
        println!();
    }
    println!(" LH (8x8):");
    for y in 0..hh {
        for x in 0..hw {
            print!("{:5} ", canvas[(hh + y) * w as usize + x]);
        }
        println!();
    }
    println!(" HH (8x8):");
    for y in 0..hh {
        for x in 0..hw {
            print!("{:5} ", canvas[(hh + y) * w as usize + hw + x]);
        }
        println!();
    }

    // Now run our decoder and dump the IDWT'd plane + differences.
    let cs = codestream::parse(OPJ16_J2K).expect("parse codestream");
    let cod = parse_cod(cs.cod.as_ref().unwrap()).expect("cod");
    let qcd = parse_qcd(cs.qcd.as_ref().unwrap(), cod.num_decomp).expect("qcd");
    let mut body = Vec::new();
    for tp in &cs.tile_parts {
        body.extend_from_slice(&OPJ16_J2K[tp.sod_offset..tp.sod_offset + tp.sod_length]);
    }
    let comp_sizes: Vec<(u32, u32, u32, u32)> = vec![(0, 0, w, h)];
    let precisions = vec![8u32];
    let params = DecodeParams {
        comp_precisions: &precisions,
    };
    let planes = decode_tile_with_params(&body, &comp_sizes, &cod, &qcd, &params).expect("tile");
    let mut dec_fwd = planes[0].clone();
    fdwt_53(&mut dec_fwd, w as usize, h as usize, w as usize);
    println!("\nforward-DWT of our decoded plane (should match reference above):");
    println!(" LL:");
    for y in 0..hh {
        for x in 0..hw {
            let v = dec_fwd[y * w as usize + x];
            let ref_v = canvas[y * w as usize + x];
            let tag = if v == ref_v { ' ' } else { '*' };
            print!("{:5}{} ", v, tag);
        }
        println!();
    }

    let sbs = build_subbands(0, 0, w, h, cod.num_decomp);
    for sb in &sbs {
        println!(
            "subband: orient={:?} band_kind={} res={} ({}, {})-({}, {})",
            sb.orient, sb.band_kind, sb.resno, sb.x0, sb.y0, sb.x1, sb.y1
        );
    }
}
