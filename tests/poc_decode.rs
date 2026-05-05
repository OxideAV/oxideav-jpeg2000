//! POC marker decoder tests (T.800 §A.6.6 / §B.12.2 / §B.12.3).
//!
//! Synthetic fixtures: we hand-inject POC marker segments into known
//! good `.j2k` codestreams (same fixtures used elsewhere) and verify
//! that decoding still yields the same image. The POC progressions are
//! chosen so the on-the-wire packet ordering is unchanged from the
//! original codestream — this exercises the POC parser, the per-tuple
//! "next layer" tracking from §B.12.2, and the per-progression dispatch
//! in the tier-2 walker without requiring a new compressed payload.
//!
//! Why hand-injection? `opj_compress`'s `-POC` option silently drops
//! the marker in many configurations (single-tile, single-layer, or
//! when the requested progressions match the COD default). We avoid
//! that brittleness by editing the codestream directly: insert one or
//! more POC marker segments after the QCD in the main header.
//!
//! The transformations all preserve the on-the-wire byte order of the
//! packet body — we only add marker bytes at the front, not reorder
//! anything. So bit-exact decode against the original PGM still holds.

use oxideav_core::CodecRegistry;
use oxideav_core::{CodecId, CodecParameters, Frame, Packet, TimeBase};

const ML3_J2K: &[u8] = include_bytes!("fixtures/opj32_3lyr.j2k");
const ML3_PGM: &[u8] = include_bytes!("fixtures/opj32_3lyr.pgm");
const ML5_J2K: &[u8] = include_bytes!("fixtures/opj32_5lyr.j2k");
const ML5_PGM: &[u8] = include_bytes!("fixtures/opj32_5lyr.pgm");
const ML_RLCP_J2K: &[u8] = include_bytes!("fixtures/opj32_3lyr_rlcp.j2k");
const ML_RLCP_PGM: &[u8] = include_bytes!("fixtures/opj32_3lyr_rlcp.pgm");
const ML_LOSSY_J2K: &[u8] = include_bytes!("fixtures/opj32_3lyr_lossy.j2k");
const ML_LOSSY_PGM: &[u8] = include_bytes!("fixtures/opj32_3lyr_lossy.pgm");

const RGB_LRCP_J2K: &[u8] = include_bytes!("fixtures/opj128_rgb_prec_lrcp.j2k");

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

fn decode_j2k(bytes: &[u8]) -> oxideav_core::VideoFrame {
    let mut reg = CodecRegistry::new();
    oxideav_jpeg2000::register_codecs(&mut reg);
    let params = CodecParameters::video(CodecId::new(oxideav_jpeg2000::CODEC_ID_STR));
    let mut dec = reg.make_decoder(&params).expect("factory");
    let pkt = Packet::new(0, TimeBase::new(1, 1), bytes.to_vec());
    dec.send_packet(&pkt).expect("send_packet");
    match dec.receive_frame().expect("receive_frame") {
        Frame::Video(v) => v,
        _ => panic!("expected video frame"),
    }
}

/// Locate the first SOT (FF 90) marker in the codestream. All main-
/// header marker segments live before this point.
fn sot_offset(j2k: &[u8]) -> usize {
    let mut i = 0;
    while i + 1 < j2k.len() {
        if j2k[i] == 0xFF && j2k[i + 1] == 0x90 {
            return i;
        }
        i += 1;
    }
    panic!("no SOT marker found");
}

/// Build a POC marker segment payload (without the leading length).
/// Each progression is `(RSpoc, CSpoc, LYEpoc, REpoc, CEpoc, Ppoc)`.
/// Assumes 8-bit component fields (`Csiz < 257`).
fn build_poc_payload(progs: &[(u8, u8, u16, u8, u8, u8)]) -> Vec<u8> {
    let mut p = Vec::with_capacity(progs.len() * 7);
    for &(rs, cs, lye, re, ce, po) in progs {
        p.push(rs);
        p.push(cs);
        p.extend_from_slice(&lye.to_be_bytes());
        p.push(re);
        p.push(ce);
        p.push(po);
    }
    p
}

/// Insert a POC marker segment into the main header of `j2k`, just
/// before the first SOT.
fn inject_main_header_poc(j2k: &[u8], progs: &[(u8, u8, u16, u8, u8, u8)]) -> Vec<u8> {
    let payload = build_poc_payload(progs);
    let lpoc = (payload.len() + 2) as u16;
    let sot = sot_offset(j2k);
    let mut out = Vec::with_capacity(j2k.len() + 4 + payload.len());
    out.extend_from_slice(&j2k[..sot]);
    out.extend_from_slice(&[0xFF, 0x5F]);
    out.extend_from_slice(&lpoc.to_be_bytes());
    out.extend_from_slice(&payload);

    // Patch the SOT's Psot to account for the added bytes? No — Psot
    // is the number of bytes from the SOT marker itself to the end of
    // the tile-part body. Inserting bytes BEFORE the SOT does not
    // change Psot. So we can copy the rest verbatim.
    out.extend_from_slice(&j2k[sot..]);
    out
}

/// Take an existing 3-layer LRCP grayscale codestream, wrap it in a
/// single-progression POC that exactly matches the implicit COD
/// progression order. Same packet stream, same image — verifies the
/// POC walker dispatches identically to the no-POC path.
#[test]
fn poc_identity_lrcp_3layer_decodes_bit_exactly() {
    // 32x32, 4 res (num_decomp=3 → res 0..=3), 1 component, 3 layers.
    let progs = [(0u8, 0u8, 3u16, 4u8, 1u8, 0u8)];
    let modified = inject_main_header_poc(ML3_J2K, &progs);
    let (_w, _h, expected) = parse_pgm(ML3_PGM);
    let vf = decode_j2k(&modified);
    let got = &vf.planes[0].data;
    let mismatches = expected
        .iter()
        .zip(got.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(mismatches, 0, "identity POC LRCP not bit-exact");
}

/// Same as above but split the layer dimension across THREE progressions:
/// each progression covers all (res, comp, prec) tuples but only one
/// new layer at a time. The per-tuple next-layer counter must advance
/// across progression boundaries (T.800 §B.12.2). The on-the-wire
/// packet order is unchanged, so the original 3-layer LRCP packet
/// stream still decodes correctly.
#[test]
fn poc_layer_split_lrcp_3layer_decodes_bit_exactly() {
    let progs = [
        (0u8, 0u8, 1u16, 4u8, 1u8, 0u8), // layer 0 only
        (0u8, 0u8, 2u16, 4u8, 1u8, 0u8), // layer 1 (skips layer 0 since already emitted)
        (0u8, 0u8, 3u16, 4u8, 1u8, 0u8), // layer 2 (skips 0 + 1)
    ];
    let modified = inject_main_header_poc(ML3_J2K, &progs);
    let (_w, _h, expected) = parse_pgm(ML3_PGM);
    let vf = decode_j2k(&modified);
    let got = &vf.planes[0].data;
    let mismatches = expected
        .iter()
        .zip(got.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(mismatches, 0, "layer-split POC LRCP not bit-exact");
}

/// 5-layer LRCP wrapped in a 5-step layer-split POC.
#[test]
fn poc_layer_split_lrcp_5layer_decodes_bit_exactly() {
    let progs = [
        (0u8, 0u8, 1u16, 4u8, 1u8, 0u8),
        (0u8, 0u8, 2u16, 4u8, 1u8, 0u8),
        (0u8, 0u8, 3u16, 4u8, 1u8, 0u8),
        (0u8, 0u8, 4u16, 4u8, 1u8, 0u8),
        (0u8, 0u8, 5u16, 4u8, 1u8, 0u8),
    ];
    let modified = inject_main_header_poc(ML5_J2K, &progs);
    let (_w, _h, expected) = parse_pgm(ML5_PGM);
    let vf = decode_j2k(&modified);
    let got = &vf.planes[0].data;
    let mismatches = expected
        .iter()
        .zip(got.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(mismatches, 0, "layer-split POC LRCP-5 not bit-exact");
}

/// 3-layer RLCP wrapped in a single-progression identity POC. This
/// progression specifies progression order 1 (RLCP), matching the COD.
#[test]
fn poc_identity_rlcp_3layer_decodes_bit_exactly() {
    let progs = [(0u8, 0u8, 3u16, 4u8, 1u8, 1u8)];
    let modified = inject_main_header_poc(ML_RLCP_J2K, &progs);
    let (_w, _h, expected) = parse_pgm(ML_RLCP_PGM);
    let vf = decode_j2k(&modified);
    let got = &vf.planes[0].data;
    let mismatches = expected
        .iter()
        .zip(got.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(mismatches, 0, "identity POC RLCP not bit-exact");
}

/// 3-layer 9/7 irreversible LRCP wrapped in identity POC.
#[test]
fn poc_identity_lossy_lrcp_3layer_decodes_bit_exactly() {
    let progs = [(0u8, 0u8, 3u16, 4u8, 1u8, 0u8)];
    let modified = inject_main_header_poc(ML_LOSSY_J2K, &progs);
    let (_w, _h, expected) = parse_pgm(ML_LOSSY_PGM);
    let vf = decode_j2k(&modified);
    let got = &vf.planes[0].data;
    let mismatches = expected
        .iter()
        .zip(got.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(mismatches, 0, "identity POC 9/7 not bit-exact");
}

/// 3-component RGB single-tile LRCP wrapped in identity POC. Verifies
/// the multi-component component-end (CEpoc) field is plumbed correctly
/// through the dispatch loops.
#[test]
fn poc_identity_lrcp_rgb_decodes_bit_exactly() {
    // Cross-check with a no-POC decode of the original codestream.
    // The 128x128 RGB single-tile LRCP fixture has 1 layer.
    let progs = [(0u8, 0u8, 1u16, 5u8, 3u8, 0u8)];
    let modified = inject_main_header_poc(RGB_LRCP_J2K, &progs);
    let original = decode_j2k(RGB_LRCP_J2K);
    let with_poc = decode_j2k(&modified);
    assert_eq!(
        original.planes.len(),
        with_poc.planes.len(),
        "plane count mismatch"
    );
    for (i, (a, b)) in original
        .planes
        .iter()
        .zip(with_poc.planes.iter())
        .enumerate()
    {
        assert_eq!(a.data.len(), b.data.len(), "plane {i} length");
        let mismatches = a
            .data
            .iter()
            .zip(b.data.iter())
            .filter(|(x, y)| x != y)
            .count();
        assert_eq!(
            mismatches, 0,
            "RGB identity POC plane {i} not bit-exact ({mismatches} mismatches)"
        );
    }
}

/// Verify the parser captures POC bytes from the main header.
#[test]
fn parser_captures_main_header_poc() {
    let progs = [(0u8, 0u8, 3u16, 4u8, 1u8, 0u8)];
    let modified = inject_main_header_poc(ML3_J2K, &progs);
    let cs = oxideav_jpeg2000::codestream::parse(&modified).expect("parse");
    let poc = cs.poc.expect("POC must be captured");
    assert_eq!(poc.len(), 7, "single-progression POC payload is 7 bytes");
    assert_eq!(poc, build_poc_payload(&progs));
}

/// Test that the POC parser rejects malformed segments (length not a
/// multiple of the per-progression entry size).
#[test]
fn parser_rejects_malformed_poc_length() {
    use oxideav_jpeg2000::decode::tile::parse_poc;
    // 5 bytes is not a multiple of 7 (8-bit comp fields).
    let bad = [0u8; 5];
    assert!(parse_poc(&bad, 1).is_err());
    // 0 bytes also rejected.
    let empty = [0u8; 0];
    assert!(parse_poc(&empty, 1).is_err());
}

/// Test POC parser rejects a progression order > 4 (Part-1 only
/// supports LRCP, RLCP, RPCL, PCRL, CPRL).
#[test]
fn parser_rejects_invalid_poc_progression() {
    use oxideav_jpeg2000::decode::tile::parse_poc;
    // Single progression with progression order = 5.
    let bad = build_poc_payload(&[(0, 0, 1, 1, 1, 5)]);
    assert!(parse_poc(&bad, 1).is_err());
}
