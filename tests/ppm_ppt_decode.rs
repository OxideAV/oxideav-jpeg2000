//! PPM / PPT marker support (T.800 §A.7.4 / §A.7.5).
//!
//! Two layers of coverage:
//!
//! - **Parser-only** tests inject hand-crafted PPM / PPT segments and
//!   verify they're captured in `Codestream::ppm` / `TilePart::ppt`.
//! - **End-to-end** tests use the body+header splitter
//!   (`oxideav_jpeg2000::decode::tile::split_packet_headers`) to
//!   re-package an existing fixture: the packet headers get extracted
//!   into a PPM (main-header) or PPT (tile-part-header) marker, the
//!   tile body keeps only the packet bodies (and any SOP markers), and
//!   the resulting codestream is decoded and compared sample-for-sample
//!   against the original fixture's decoded output.

use oxideav_core::Frame;
use oxideav_jpeg2000::codestream;
use oxideav_jpeg2000::decode::frame::decode_frame;
use oxideav_jpeg2000::decode::tile::{parse_cod, parse_poc, parse_qcd, split_packet_headers};

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

// ---- End-to-end round-trip tests ------------------------------------------

const J2K_TILED_GRAY: &[u8] = include_bytes!("fixtures/opj128_gray_tiled_prec_lrcp.j2k");

/// Pull a frame's flat sample buffer for comparison. Asserts both
/// frames have identical geometry and pixel format and returns one
/// bag of bytes per plane.
fn frame_planes(frame: &Frame) -> Vec<Vec<u8>> {
    match frame {
        Frame::Video(v) => v.planes.iter().map(|p| p.data.clone()).collect(),
        _ => panic!("expected video frame"),
    }
}

/// Assert two decoded frames are sample-for-sample identical.
fn assert_frames_equal(label: &str, a: &Frame, b: &Frame) {
    let (Frame::Video(va), Frame::Video(vb)) = (a, b) else {
        panic!("{label}: not a video frame");
    };
    assert_eq!(va.format, vb.format, "{label}: pixel format mismatch");
    assert_eq!(va.width, vb.width, "{label}: width mismatch");
    assert_eq!(va.height, vb.height, "{label}: height mismatch");
    assert_eq!(
        va.planes.len(),
        vb.planes.len(),
        "{label}: plane count mismatch"
    );
    let pa = frame_planes(a);
    let pb = frame_planes(b);
    for (i, (la, lb)) in pa.iter().zip(pb.iter()).enumerate() {
        assert_eq!(la, lb, "{label}: plane {i} sample mismatch");
    }
}

/// Compute per-tile-component bounds for a tile (cx0, cy0, cx1, cy1)
/// in component coordinates, mirroring `decode::frame::decode_frame`.
fn tile_comp_sizes(siz: &codestream::Siz, tile_idx: u32) -> Vec<(u32, u32, u32, u32)> {
    let xtsiz = siz.xtsiz;
    let ytsiz = siz.ytsiz;
    let xtosiz = siz.xtosiz;
    let ytosiz = siz.ytosiz;
    let xosiz = siz.xosiz;
    let yosiz = siz.yosiz;
    let xsiz = siz.xsiz;
    let ysiz = siz.ysiz;
    let nx = ((xsiz - xtosiz).div_ceil(xtsiz)).max(1);
    let p = tile_idx % nx;
    let q = tile_idx / nx;
    let tx0 = (xtosiz + p * xtsiz).max(xosiz);
    let ty0 = (ytosiz + q * ytsiz).max(yosiz);
    let tx1 = (xtosiz + (p + 1) * xtsiz).min(xsiz);
    let ty1 = (ytosiz + (q + 1) * ytsiz).min(ysiz);
    siz.components
        .iter()
        .map(|c| {
            let xr = c.xrsiz as u32;
            let yr = c.yrsiz as u32;
            let cx0 = tx0.div_ceil(xr);
            let cy0 = ty0.div_ceil(yr);
            let cx1 = tx1.div_ceil(xr);
            let cy1 = ty1.div_ceil(yr);
            // Tile decoder treats the rectangle as tile-local — pass
            // (0, 0, w, h) so the layout matches the body.
            (0, 0, cx1 - cx0, cy1 - cy0)
        })
        .collect()
}

/// Locate the SOT (FF 90) marker that opens the tile-part whose body
/// begins at `sod_offset`. Walks backward from the SOD marker; the SOT
/// marker is the most recent `FF 90` byte-pair preceding it.
fn find_sot_marker_off(j2k: &[u8], sod_offset: usize) -> usize {
    // SOD marker is at sod_offset - 2; SOT lives somewhere before it
    // (with possibly other tile-part-header marker segments between).
    let mut i = sod_offset - 2;
    while i >= 1 {
        if j2k[i - 1] == 0xFF && j2k[i] == 0x90 {
            return i - 1;
        }
        i -= 1;
    }
    panic!("no SOT before SOD at offset {sod_offset}");
}

/// Re-emit the codestream replacing each tile-part's body with
/// `new_bodies[i]`. Optionally insert `before_sot_extra` (e.g. a PPM
/// marker segment) into the main header just before the first SOT.
/// Each tile-part's SOT `Psot` is patched to reflect the new size.
fn rebuild_with_main_header_extra_and_body(
    j2k: &[u8],
    cs: &codestream::Codestream,
    before_sot_extra: &[u8],
    new_bodies: &[Vec<u8>],
) -> Vec<u8> {
    assert_eq!(
        new_bodies.len(),
        cs.tile_parts.len(),
        "one body per tile-part"
    );
    let first_sot = find_sot_marker_off(j2k, cs.tile_parts[0].sod_offset);
    let mut out = Vec::with_capacity(j2k.len() + before_sot_extra.len());
    out.extend_from_slice(&j2k[..first_sot]);
    out.extend_from_slice(before_sot_extra);
    for (i, tp) in cs.tile_parts.iter().enumerate() {
        let sot_marker_off = find_sot_marker_off(j2k, tp.sod_offset);
        let header_end = tp.sod_offset; // after SOD's 2-byte marker
        let new_body = &new_bodies[i];
        let sot_pos_in_out = out.len();
        out.extend_from_slice(&j2k[sot_marker_off..header_end]);
        // Psot covers from SOT marker through end of body.
        let header_len = header_end - sot_marker_off;
        let new_psot = (header_len + new_body.len()) as u32;
        let psot_off = sot_pos_in_out + 6;
        out[psot_off..psot_off + 4].copy_from_slice(&new_psot.to_be_bytes());
        out.extend_from_slice(new_body);
    }
    let last = cs.tile_parts.last().unwrap();
    let tail_start = last.sod_offset + last.sod_length;
    out.extend_from_slice(&j2k[tail_start..]);
    out
}

/// Same as above but inserts a PPT marker per tile-part (just before
/// SOD) instead of a global PPM marker.
fn rebuild_with_per_tile_ppt_and_body(
    j2k: &[u8],
    cs: &codestream::Codestream,
    ppts: &[Vec<u8>],
    new_bodies: &[Vec<u8>],
) -> Vec<u8> {
    assert_eq!(ppts.len(), cs.tile_parts.len());
    assert_eq!(new_bodies.len(), cs.tile_parts.len());
    let first_sot = find_sot_marker_off(j2k, cs.tile_parts[0].sod_offset);
    let mut out = Vec::with_capacity(j2k.len() + ppts.iter().map(|p| p.len() + 4).sum::<usize>());
    out.extend_from_slice(&j2k[..first_sot]);
    for (i, tp) in cs.tile_parts.iter().enumerate() {
        let sot_marker_off = find_sot_marker_off(j2k, tp.sod_offset);
        let sod_marker_off = tp.sod_offset - 2;
        let new_body = &new_bodies[i];
        let ppt = &ppts[i];
        let sot_pos_in_out = out.len();
        // Copy SOT marker + any other tile-part-header markers up to
        // (but not including) SOD.
        let header_pre_sod_len = sod_marker_off - sot_marker_off;
        out.extend_from_slice(&j2k[sot_marker_off..sod_marker_off]);
        // Insert PPT segment: FF 61 [Lppt BE u16] [Zppt=0] [Ippt...]
        let lppt = (ppt.len() + 2) as u16;
        out.extend_from_slice(&[0xFF, 0x61]);
        out.extend_from_slice(&lppt.to_be_bytes());
        out.extend_from_slice(ppt);
        // Copy SOD marker.
        out.extend_from_slice(&j2k[sod_marker_off..sod_marker_off + 2]);
        // Append new body.
        out.extend_from_slice(new_body);
        // Patch Psot. SOT marker through end of body.
        let new_psot = (header_pre_sod_len + 4 + ppt.len() + 2 + new_body.len()) as u32;
        let psot_off = sot_pos_in_out + 6;
        out[psot_off..psot_off + 4].copy_from_slice(&new_psot.to_be_bytes());
    }
    let last = cs.tile_parts.last().unwrap();
    let tail_start = last.sod_offset + last.sod_length;
    out.extend_from_slice(&j2k[tail_start..]);
    out
}

/// Run the splitter on every tile-part of `cs` and return per-tile-part
/// `(headers, body_only)` pairs. Asserts each tile has exactly one
/// tile-part (the OPJ default for our fixtures), so `tile_idx` and the
/// tile-part index coincide.
fn split_all_tile_parts(j2k: &[u8], cs: &codestream::Codestream) -> Vec<(Vec<u8>, Vec<u8>)> {
    let cod = parse_cod(cs.cod.as_ref().expect("COD")).expect("parse_cod");
    let qcd = parse_qcd(cs.qcd.as_ref().expect("QCD"), cod.num_decomp).expect("parse_qcd");
    let main_poc = cs
        .poc
        .as_ref()
        .map(|b| parse_poc(b, cs.siz.components.len() as u16).expect("parse_poc"));

    // Verify single-tile-part-per-tile assumption: each tile_index
    // appears at most once across the codestream's tile_parts vec.
    let mut seen = std::collections::HashSet::new();
    for tp in &cs.tile_parts {
        assert!(
            seen.insert(tp.tile_index),
            "splitter test fixture has multiple tile-parts per tile"
        );
    }

    cs.tile_parts
        .iter()
        .map(|tp| {
            let body = &j2k[tp.sod_offset..tp.sod_offset + tp.sod_length];
            let comp_sizes = tile_comp_sizes(&cs.siz, tp.tile_index as u32);
            // Per-tile POC (tile-part header takes precedence over main).
            let tp_poc = tp
                .poc
                .as_ref()
                .map(|b| parse_poc(b, cs.siz.components.len() as u16).expect("parse_poc"));
            let poc = tp_poc.as_ref().or(main_poc.as_ref());
            split_packet_headers(body, &comp_sizes, &cod, &qcd, poc).expect("split")
        })
        .collect()
}

/// Build a PPM payload: `[Zppm][Nppm_0 BE u32][headers_0...][Nppm_1...]...`.
fn build_ppm_payload(per_tp_headers: &[Vec<u8>], zppm: u8) -> Vec<u8> {
    let mut payload =
        Vec::with_capacity(1 + per_tp_headers.iter().map(|h| 4 + h.len()).sum::<usize>());
    payload.push(zppm);
    for h in per_tp_headers {
        payload.extend_from_slice(&(h.len() as u32).to_be_bytes());
        payload.extend_from_slice(h);
    }
    payload
}

/// PPM round-trip on the 3-layer single-tile fixture: split headers
/// out, inject as PPM, decode, compare to the original.
#[test]
fn ppm_round_trip_single_tile_3layer() {
    let cs = codestream::parse(J2K_3LYR).expect("parse");
    assert_eq!(cs.tile_parts.len(), 1, "fixture is single tile-part");
    let original = decode_frame(&cs, J2K_3LYR).expect("decode original");

    let splits = split_all_tile_parts(J2K_3LYR, &cs);
    let headers_per_tp: Vec<Vec<u8>> = splits.iter().map(|(h, _)| h.clone()).collect();
    let bodies_per_tp: Vec<Vec<u8>> = splits.iter().map(|(_, b)| b.clone()).collect();

    let ppm_payload = build_ppm_payload(&headers_per_tp, 0);
    let lppm = (ppm_payload.len() + 2) as u16;
    let mut ppm_segment = Vec::with_capacity(4 + ppm_payload.len());
    ppm_segment.extend_from_slice(&[0xFF, 0x60]);
    ppm_segment.extend_from_slice(&lppm.to_be_bytes());
    ppm_segment.extend_from_slice(&ppm_payload);

    let modified =
        rebuild_with_main_header_extra_and_body(J2K_3LYR, &cs, &ppm_segment, &bodies_per_tp);

    let cs2 = codestream::parse(&modified).expect("re-parse modified");
    assert_eq!(
        cs2.ppm.len(),
        1,
        "modified codestream carries one PPM segment"
    );
    let decoded = decode_frame(&cs2, &modified).expect("decode modified");
    assert_frames_equal("ppm_round_trip_3layer", &original, &decoded);
}

/// PPM round-trip with the headers split across two PPM segments
/// (Zppm = 0 and Zppm = 1) — exercises the multi-segment unpacker.
#[test]
fn ppm_round_trip_two_segments() {
    let cs = codestream::parse(J2K_3LYR).expect("parse");
    let original = decode_frame(&cs, J2K_3LYR).expect("decode original");

    let splits = split_all_tile_parts(J2K_3LYR, &cs);
    let headers_per_tp: Vec<Vec<u8>> = splits.iter().map(|(h, _)| h.clone()).collect();
    let bodies_per_tp: Vec<Vec<u8>> = splits.iter().map(|(_, b)| b.clone()).collect();

    // Build the full PPM payload, then split it after Zppm into two
    // halves and emit them as Zppm=0 and Zppm=1 segments. The decoder
    // sorts by Zppm and concatenates so the result is identical.
    let full = build_ppm_payload(&headers_per_tp, 0);
    let mid = full.len() / 2;
    let head = &full[1..mid]; // skip leading Zppm=0
    let tail = &full[mid..];
    let mut payload0 = vec![0u8];
    payload0.extend_from_slice(head);
    let mut payload1 = vec![1u8];
    payload1.extend_from_slice(tail);

    let mut extra = Vec::new();
    for payload in [&payload0, &payload1] {
        let lppm = (payload.len() + 2) as u16;
        extra.extend_from_slice(&[0xFF, 0x60]);
        extra.extend_from_slice(&lppm.to_be_bytes());
        extra.extend_from_slice(payload);
    }

    let modified = rebuild_with_main_header_extra_and_body(J2K_3LYR, &cs, &extra, &bodies_per_tp);
    let cs2 = codestream::parse(&modified).expect("re-parse two-segment ppm");
    assert_eq!(cs2.ppm.len(), 2, "two PPM segments captured");
    let decoded = decode_frame(&cs2, &modified).expect("decode two-segment ppm");
    assert_frames_equal("ppm_round_trip_two_segments", &original, &decoded);
}

/// PPT round-trip on a multi-tile gray fixture: each tile-part gets
/// its own PPT marker with that tile's packet headers. Decoded result
/// must match the original.
#[test]
fn ppt_round_trip_multi_tile_gray() {
    let cs = codestream::parse(J2K_TILED_GRAY).expect("parse");
    assert!(cs.tile_parts.len() >= 2, "fixture must have multiple tiles");
    let original = decode_frame(&cs, J2K_TILED_GRAY).expect("decode original");

    let splits = split_all_tile_parts(J2K_TILED_GRAY, &cs);
    let headers_per_tp: Vec<Vec<u8>> = splits.iter().map(|(h, _)| h.clone()).collect();
    let bodies_per_tp: Vec<Vec<u8>> = splits.iter().map(|(_, b)| b.clone()).collect();

    // Build per-tile-part PPT payload: leading Zppt=0 then headers.
    let ppts: Vec<Vec<u8>> = headers_per_tp
        .iter()
        .map(|h| {
            let mut v = Vec::with_capacity(1 + h.len());
            v.push(0u8);
            v.extend_from_slice(h);
            v
        })
        .collect();

    let modified = rebuild_with_per_tile_ppt_and_body(J2K_TILED_GRAY, &cs, &ppts, &bodies_per_tp);
    let cs2 = codestream::parse(&modified).expect("re-parse PPT codestream");
    for tp in &cs2.tile_parts {
        assert_eq!(tp.ppt.len(), 1, "each tile-part carries one PPT");
    }
    let decoded = decode_frame(&cs2, &modified).expect("decode PPT codestream");
    assert_frames_equal("ppt_round_trip_multi_tile_gray", &original, &decoded);
}
