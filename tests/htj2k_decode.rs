//! End-to-end HTJ2K (ITU-T T.814 | ISO/IEC 15444-15) decode tests.
//!
//! Each fixture is a `.j2c` HT codestream produced by the black-box
//! validator `ojph_compress` and a reference reconstruction produced by
//! `ojph_expand`; the test decodes the codestream with this crate and
//! asserts the recovered samples match the reference bit-for-bit (for
//! reversible streams) or exactly (these lossy fixtures reconstruct
//! identically here). The validators are opaque processes — their source
//! is never consulted.

/// Locate the start of the binary raster in a Netpbm file (byte after
/// the third newline) and return `(width, height, data_start)`.
fn pnm_geometry(b: &[u8]) -> (usize, usize, usize) {
    let mut nl = 0;
    let mut i = 0;
    while nl < 3 {
        if b[i] == b'\n' {
            nl += 1;
        }
        i += 1;
    }
    let header = std::str::from_utf8(&b[3..i]).unwrap();
    let mut it = header.split_whitespace();
    let w: usize = it.next().unwrap().parse().unwrap();
    let h: usize = it.next().unwrap().parse().unwrap();
    (w, h, i)
}

/// Parse a binary `P5` (grayscale) PGM into `(w, h, samples)`.
fn parse_pgm(b: &[u8]) -> (usize, usize, Vec<i32>) {
    let (w, h, start) = pnm_geometry(b);
    let data = b[start..start + w * h].iter().map(|&x| x as i32).collect();
    (w, h, data)
}

/// Parse a binary `P6` (RGB) PPM into `(w, h, interleaved_samples)`.
fn parse_ppm(b: &[u8]) -> (usize, usize, Vec<i32>) {
    let (w, h, start) = pnm_geometry(b);
    let data = b[start..start + w * h * 3]
        .iter()
        .map(|&x| x as i32)
        .collect();
    (w, h, data)
}

#[test]
fn ht_8x8_rev_1decomp_matches_ojph() {
    let bytes = include_bytes!("fixtures/ht_8x8_rev_1decomp.j2c");
    let refpgm = include_bytes!("fixtures/ht_8x8_rev_1decomp.pgm");
    let (rw, rh, rdata) = parse_pgm(refpgm);
    let img = oxideav_jpeg2000::decode_j2k(bytes).expect("decode");
    let c = &img.components[0];
    assert_eq!((c.width as usize, c.height as usize), (rw, rh));
    assert_eq!(c.samples, rdata, "grayscale 8x8 reconstruction differs");
}

#[test]
fn ht_gray32_d3_matches_ojph() {
    let bytes = include_bytes!("fixtures/ht_gray32_d3.j2c");
    let refpgm = include_bytes!("fixtures/ht_gray32_d3_ref.pgm");
    let (rw, rh, rdata) = parse_pgm(refpgm);
    let img = oxideav_jpeg2000::decode_j2k(bytes).expect("decode");
    assert_eq!(img.components.len(), 1);
    let c = &img.components[0];
    assert_eq!((c.width as usize, c.height as usize), (rw, rh));
    assert_eq!(
        c.samples, rdata,
        "grayscale 32x24 / 3-decomp reconstruction differs"
    );
}

#[test]
fn ht_rgb24_rev_matches_ojph() {
    let bytes = include_bytes!("fixtures/ht_rgb24_rev.j2c");
    let refppm = include_bytes!("fixtures/ht_rgb24_rev_ref.ppm");
    let (rw, rh, rdata) = parse_ppm(refppm);
    let img = oxideav_jpeg2000::decode_j2k(bytes).expect("decode");
    assert_eq!(img.components.len(), 3);
    // ojph PPM is interleaved RGB; our components are planar.
    for (comp, c) in img.components.iter().enumerate() {
        assert_eq!((c.width as usize, c.height as usize), (rw, rh));
        let de_interleaved: Vec<i32> = rdata.iter().skip(comp).step_by(3).copied().collect();
        assert_eq!(
            c.samples, de_interleaved,
            "RGB component {comp} reconstruction differs"
        );
    }
}

#[test]
fn ht_gray32_irreversible_matches_ojph() {
    // Lossy irreversible (9-7) HT: our reconstruction must match the
    // ojph reference reconstruction exactly (both apply the same §E.1
    // midpoint reconstruction to the same decoded coefficients).
    let bytes = include_bytes!("fixtures/ht_gray32_irv.j2c");
    let refpgm = include_bytes!("fixtures/ht_gray32_irv_ref.pgm");
    let (rw, rh, rdata) = parse_pgm(refpgm);
    let img = oxideav_jpeg2000::decode_j2k(bytes).expect("decode");
    let c = &img.components[0];
    assert_eq!((c.width as usize, c.height as usize), (rw, rh));
    assert_eq!(
        c.samples, rdata,
        "irreversible reconstruction differs from ojph"
    );
}
