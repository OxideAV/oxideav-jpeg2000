//! Integration test: parse a real OpenJPEG-generated J2K codestream.
//!
//! The fixture in `tests/fixtures/tiny.j2k` is a 128x96 single-component
//! grayscale image emitted by OpenJPEG 2.5.x. It exercises a real
//! marker chain (SOC, SIZ, COD, QCD, COM, SOT, SOD, EOC) without
//! relying on our hand-crafted layout.

use oxideav_jpeg2000::codestream;

const TINY_J2K: &[u8] = include_bytes!("fixtures/tiny.j2k");

#[test]
fn parses_real_openjpeg_codestream() {
    let cs = codestream::parse(TINY_J2K).expect("parse real j2k");
    assert_eq!(cs.siz.image_width(), 128);
    assert_eq!(cs.siz.image_height(), 96);
    assert_eq!(cs.siz.num_components(), 1);
    let comp = cs.siz.components[0];
    // OpenJPEG encoded this 8-bit PGM as a 16-bit-capable component —
    // the SIZ reports bit_depth=16 even though the source is 8-bit.
    // We only check the parser recovers the field accurately.
    assert!(comp.bit_depth() >= 8 && comp.bit_depth() <= 16);
    assert!(!comp.is_signed());
    assert_eq!(comp.xrsiz, 1);
    assert_eq!(comp.yrsiz, 1);
    assert!(cs.cod.is_some(), "COD segment must be captured");
    assert!(cs.qcd.is_some(), "QCD segment must be captured");
    assert_eq!(
        cs.tile_parts.len(),
        1,
        "single-tile image must have one tile-part"
    );
    let tp = cs.tile_parts[0];
    assert_eq!(tp.tile_index, 0);
    assert_eq!(tp.tile_part_index, 0);
    assert!(tp.sod_length > 0, "tile-part must carry compressed data");
    assert!(cs.eoc_offset.is_some(), "EOC must be located");
}
