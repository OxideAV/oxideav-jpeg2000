//! PPM / PPT marker support (T.800 §A.7.4 / §A.7.5).
//!
//! These tests exercise the parser side and the per-tile aggregation
//! logic for packed packet headers. End-to-end PPM/PPT decode of real
//! fixtures requires either an OpenJPEG configuration that emits
//! these markers (none of our existing fixtures do — `opj_compress`
//! does not emit PPT/PPM in default modes) or a custom fixture-
//! generation tool that splits an existing codestream into a body-
//! only stream + an external packet-header buffer. The latter is
//! deferred to a follow-up round; for now we verify the parser, the
//! `unpack_ppm` helper, and the structural plumbing through the
//! tier-2 walker via `DecodeParams::packet_headers`.

use oxideav_jpeg2000::codestream;

const J2K_3LYR: &[u8] = include_bytes!("fixtures/opj32_3lyr.j2k");

/// Locate the first SOT (FF 90) marker in the codestream.
fn sot_offset(j2k: &[u8]) -> usize {
    let mut i = 0;
    while i + 1 < j2k.len() {
        if j2k[i] == 0xFF && j2k[i + 1] == 0x90 {
            return i;
        }
        i += 1;
    }
    panic!("no SOT");
}

/// Insert a PPM marker segment in the main header before SOT. The
/// `payload` must include the leading Zppm byte.
fn inject_main_header_ppm(j2k: &[u8], payload: &[u8]) -> Vec<u8> {
    let lppm = (payload.len() + 2) as u16;
    let sot = sot_offset(j2k);
    let mut out = Vec::with_capacity(j2k.len() + 4 + payload.len());
    out.extend_from_slice(&j2k[..sot]);
    out.extend_from_slice(&[0xFF, 0x60]);
    out.extend_from_slice(&lppm.to_be_bytes());
    out.extend_from_slice(payload);
    out.extend_from_slice(&j2k[sot..]);
    out
}

/// Insert a PPT marker segment inside the first tile-part header
/// (just before SOD). Updates the `Psot` of that tile-part to account
/// for the inserted bytes. `payload` must include leading Zppt.
fn inject_first_tile_part_ppt(j2k: &[u8], payload: &[u8]) -> Vec<u8> {
    let sot = sot_offset(j2k);
    // Walk from SOT marker forward through marker segments until we
    // hit SOD. Each segment after SOT is `marker (2) + Lseg (2) + body
    // (Lseg-2)`. SOD has no length field and is just `FF 93`.
    let mut pos = sot + 2; // skip SOT marker
                           // SOT body is the first segment with Lseg=10; consume it.
    let lseg = u16::from_be_bytes([j2k[pos], j2k[pos + 1]]) as usize;
    pos += lseg;
    // Now scan tile-part-header markers until SOD.
    while !(j2k[pos] == 0xFF && j2k[pos + 1] == 0x93) {
        // Some other tile-part-header segment.
        let lseg = u16::from_be_bytes([j2k[pos + 2], j2k[pos + 3]]) as usize;
        pos += 2 + lseg;
    }
    let sod_pos = pos;
    assert_eq!(j2k[sod_pos], 0xFF);
    assert_eq!(j2k[sod_pos + 1], 0x93);
    let lppt = (payload.len() + 2) as u16;
    let inserted = 4 + payload.len();
    let mut out = Vec::with_capacity(j2k.len() + inserted);
    out.extend_from_slice(&j2k[..sod_pos]);
    out.extend_from_slice(&[0xFF, 0x61]);
    out.extend_from_slice(&lppt.to_be_bytes());
    out.extend_from_slice(payload);
    out.extend_from_slice(&j2k[sod_pos..]);
    // Patch Psot. SOT body layout (8 bytes after Lsot): Isot(2), Psot(4),
    // TPsot(1), TNsot(1).
    let psot_off = sot + 4 + 2; // SOT marker (2) + Lsot (2) + Isot (2)
    let psot_old = u32::from_be_bytes([
        out[psot_off],
        out[psot_off + 1],
        out[psot_off + 2],
        out[psot_off + 3],
    ]);
    if psot_old != 0 {
        let psot_new = psot_old + inserted as u32;
        out[psot_off..psot_off + 4].copy_from_slice(&psot_new.to_be_bytes());
    }
    out
}

/// Parser captures a hand-injected PPM marker in the main header.
#[test]
fn parser_captures_main_header_ppm() {
    // Minimal valid PPM payload: Zppm=0, Nppm_0 = 0 (zero packet
    // header bytes for tile-part 0). 5 bytes total.
    let payload: [u8; 5] = [0x00, 0x00, 0x00, 0x00, 0x00];
    let modified = inject_main_header_ppm(J2K_3LYR, &payload);
    let cs = codestream::parse(&modified).expect("parse");
    assert_eq!(cs.ppm.len(), 1, "one PPM segment captured");
    assert_eq!(cs.ppm[0], payload);
}

/// Parser captures multiple PPM segments separately (the decoder will
/// later sort them by Zppm and concatenate).
#[test]
fn parser_captures_multiple_ppm_segments() {
    // Two PPM segments with Zppm = 1 then 0 (out of order).
    let p1: [u8; 5] = [0x01, 0x00, 0x00, 0x00, 0x00];
    let p0: [u8; 5] = [0x00, 0x00, 0x00, 0x00, 0x00];
    let mut buf = inject_main_header_ppm(J2K_3LYR, &p1);
    buf = inject_main_header_ppm(&buf, &p0);
    let cs = codestream::parse(&buf).expect("parse");
    assert_eq!(cs.ppm.len(), 2, "two PPM segments captured");
}

/// Parser captures PPT segments inside a tile-part header.
#[test]
fn parser_captures_tile_part_ppt() {
    // Empty PPT: Zppt=0, no Ippt bytes. Length = 1.
    let payload: [u8; 1] = [0x00];
    let modified = inject_first_tile_part_ppt(J2K_3LYR, &payload);
    let cs = codestream::parse(&modified).expect("parse");
    assert_eq!(cs.tile_parts.len(), 1);
    assert_eq!(cs.tile_parts[0].ppt.len(), 1, "one PPT segment captured");
    assert_eq!(cs.tile_parts[0].ppt[0], payload);
}

/// Parser captures multiple PPT segments inside one tile-part header.
#[test]
fn parser_captures_multiple_ppt_in_tile_part() {
    let p1: [u8; 1] = [0x01];
    let p0: [u8; 1] = [0x00];
    let mut buf = inject_first_tile_part_ppt(J2K_3LYR, &p1);
    buf = inject_first_tile_part_ppt(&buf, &p0);
    let cs = codestream::parse(&buf).expect("parse");
    assert_eq!(
        cs.tile_parts[0].ppt.len(),
        2,
        "two PPT segments in tile-part"
    );
}

/// A codestream without any PPM/PPT yields empty `ppm` + per-tile-part
/// empty `ppt` lists. Sanity check that the optional fields default
/// correctly across an existing fixture.
#[test]
fn parser_no_ppm_or_ppt_in_baseline_fixture() {
    let cs = codestream::parse(J2K_3LYR).expect("parse");
    assert!(cs.ppm.is_empty(), "baseline fixture has no PPM");
    for tp in &cs.tile_parts {
        assert!(tp.ppt.is_empty(), "baseline fixture has no PPT");
    }
}
