//! **Round-6 diff harness.** Traces every MQ `decode()` call on both
//! sides of the OPJ-interop boundary: (a) decoding OpenJPEG's
//! `opj16_l1.j2k` LL code-block, (b) encoding the forward-5/3-DWT of
//! the same reference. The printed events share a stable schema
//! (`pass bpno=… x=… y=… ctx=… bit=…`) so a `diff` between the two
//! streams points directly at the pass/sample where our encoder
//! disagrees with what a spec-conformant decoder would accept.
//!
//! Run with:
//!
//! ```bash
//! cargo test --test opj_t1_mqtrace -- --ignored --nocapture
//! ```

use oxideav_jpeg2000::decode::t1::{decode_cblk, trace as dec_trace, Orient};
use oxideav_jpeg2000::encode::dwt::fdwt_53;
use oxideav_jpeg2000::encode::t1::encode_cblk;

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

/// Extract the LL code-block from the opj16 fixture by running the
/// tier-2 layer parser. Returns `(w, h, band_numbps, bpno_start,
/// total_passes, data_bytes)`.
fn extract_ll_cblk() -> (usize, usize, i32, i32, u32, Vec<u8>) {
    oxideav_jpeg2000::decode::tile::extract_ll_cblk_round6(OPJ16_J2K).expect("extract LL cblk")
}

/// Extract any of the resolution-1 sub-band code-blocks (`band_kind`
/// in `1..=3`: HL=1, LH=2, HH=3).
fn extract_sb_cblk(band_kind: u8) -> (usize, usize, i32, i32, u32, Vec<u8>) {
    oxideav_jpeg2000::decode::tile::extract_sb_cblk_round6(OPJ16_J2K, band_kind)
        .expect("extract sb cblk")
}

fn trace_decode_ll() -> Vec<String> {
    let (w, h, band_numbps, bpno_start, total_passes, data) = extract_ll_cblk();
    let _ = band_numbps;
    dec_trace::enable();
    let _ = decode_cblk(data, w, h, bpno_start, total_passes, Orient::Ll, 0);
    dec_trace::take().unwrap_or_default()
}

fn trace_encode_ll() -> Vec<String> {
    // Re-encode the LL code-block from the reference PGM's forward DWT.
    let (w, h, pgm) = parse_pgm(OPJ16_PGM);
    assert_eq!((w, h), (16, 16));
    let mut canvas: Vec<i32> = pgm.iter().map(|&b| b as i32 - 128).collect();
    fdwt_53(&mut canvas, w as usize, h as usize, w as usize);
    // LL is top-left 8x8.
    let hw = (w / 2) as usize;
    let hh = (h / 2) as usize;
    let mut ll = Vec::with_capacity(hw * hh);
    for y in 0..hh {
        for x in 0..hw {
            ll.push(canvas[y * w as usize + x]);
        }
    }
    dec_trace::enable();
    let _ = encode_cblk(&ll, hw, hh, 8, Orient::Ll);
    dec_trace::take().unwrap_or_default()
}

fn trace_decode_sb(band_kind: u8) -> Vec<String> {
    let (w, h, band_numbps, bpno_start, total_passes, data) = extract_sb_cblk(band_kind);
    let _ = band_numbps;
    let orient = match band_kind {
        1 => Orient::Hl,
        2 => Orient::Ll, // LH uses LL context (Table D-2 row for "LL and LH")
        3 => Orient::Hh,
        _ => panic!("band_kind must be 1..=3"),
    };
    dec_trace::enable();
    let _ = decode_cblk(data, w, h, bpno_start, total_passes, orient, 0);
    dec_trace::take().unwrap_or_default()
}

fn trace_encode_sb(band_kind: u8) -> Vec<String> {
    let (w, h, pgm) = parse_pgm(OPJ16_PGM);
    let mut canvas: Vec<i32> = pgm.iter().map(|&b| b as i32 - 128).collect();
    fdwt_53(&mut canvas, w as usize, h as usize, w as usize);
    let hw = (w / 2) as usize;
    let hh = (h / 2) as usize;
    let (src_x0, src_y0, orient, band_numbps) = match band_kind {
        1 => (hw, 0, Orient::Hl, 10i32),
        2 => (0, hh, Orient::Ll, 10i32),
        3 => (hw, hh, Orient::Hh, 11i32),
        _ => panic!("band_kind must be 1..=3"),
    };
    let mut sb = Vec::with_capacity(hw * hh);
    for y in 0..hh {
        for x in 0..hw {
            sb.push(canvas[(src_y0 + y) * w as usize + (src_x0 + x)]);
        }
    }
    dec_trace::enable();
    let _ = encode_cblk(&sb, hw, hh, band_numbps, orient);
    dec_trace::take().unwrap_or_default()
}

#[test]
#[ignore = "diagnostic; emits MQ bit traces for HH code-block round-6 diff"]
fn diff_opj16_hh_mqtrace() {
    let dec = trace_decode_sb(3);
    let enc = trace_encode_sb(3);
    let n = dec.len().min(enc.len());
    eprintln!("HH decoder={}, encoder={}", dec.len(), enc.len());
    eprintln!("first decoder events:");
    for (i, e) in dec.iter().take(8).enumerate() {
        eprintln!("  DEC {i:4}  {e}");
    }
    eprintln!("first encoder events:");
    for (i, e) in enc.iter().take(8).enumerate() {
        eprintln!("  ENC {i:4}  {e}");
    }
    let mut first_diff = None;
    for i in 0..n {
        if dec[i] != enc[i] {
            first_diff = Some(i);
            break;
        }
    }
    if let Some(i) = first_diff {
        eprintln!("HH first divergence at #{i}:");
        for j in i.saturating_sub(3)..(i + 4).min(n) {
            let m = if j == i { ">>>" } else { "   " };
            eprintln!("{m} {j:4}  DEC {}", dec[j]);
            eprintln!("{m} {j:4}  ENC {}", enc[j]);
        }
    } else {
        eprintln!("HH no divergence ({n} events)");
    }
}

#[test]
#[ignore = "diagnostic; emits MQ bit traces for HL code-block round-6 diff"]
fn diff_opj16_hl_mqtrace() {
    let dec = trace_decode_sb(1);
    let enc = trace_encode_sb(1);
    let n = dec.len().min(enc.len());
    eprintln!("HL decoder={}, encoder={}", dec.len(), enc.len());
    let mut first_diff = None;
    for i in 0..n {
        if dec[i] != enc[i] {
            first_diff = Some(i);
            break;
        }
    }
    if let Some(i) = first_diff {
        eprintln!("HL first divergence at #{i}");
        for j in i.saturating_sub(3)..(i + 4).min(n) {
            eprintln!("  {j:4}  DEC {}", dec[j]);
            eprintln!("  {j:4}  ENC {}", enc[j]);
        }
    } else {
        eprintln!("HL no divergence ({n} events)");
    }
}

#[test]
#[ignore = "diagnostic; emits MQ bit traces for the LL code-block round-6 diff"]
fn diff_opj16_ll_mqtrace() {
    let dec_events = trace_decode_ll();
    let enc_events = trace_encode_ll();
    let n = dec_events.len().min(enc_events.len());
    eprintln!(
        "decoder events = {}, encoder events = {}",
        dec_events.len(),
        enc_events.len()
    );
    let mut first_diff: Option<usize> = None;
    for i in 0..n {
        if dec_events[i] != enc_events[i] {
            first_diff = Some(i);
            break;
        }
    }
    if let Some(i) = first_diff {
        eprintln!("first divergence at event #{i}:");
        let ctx_lo = i.saturating_sub(3);
        let ctx_hi = (i + 4).min(n);
        for j in ctx_lo..ctx_hi {
            let marker = if j == i { ">>> " } else { "    " };
            eprintln!("{marker}{j:4}  DEC {}", dec_events[j]);
            eprintln!("{marker}{j:4}  ENC {}", enc_events[j]);
        }
    } else {
        eprintln!("no divergence in common prefix ({n} events)");
    }
    // Also dump the first/last few events for sanity checking.
    eprintln!("\nfirst 8 decoder events:");
    for (i, e) in dec_events.iter().take(8).enumerate() {
        eprintln!("  {i:4}  {e}");
    }
    eprintln!("last 8 decoder events:");
    for (i, e) in dec_events
        .iter()
        .enumerate()
        .skip(dec_events.len().saturating_sub(8))
    {
        eprintln!("  {i:4}  {e}");
    }
    eprintln!("pass transitions in decoder stream:");
    let mut prev = String::new();
    for (i, e) in dec_events.iter().enumerate() {
        let key: String = e.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
        if key != prev {
            eprintln!("  {i:4}  [{key}] -> first event: {e}");
            prev = key;
        }
    }
    eprintln!("\n=== DEC events touching (0,2) ===");
    for (i, e) in dec_events.iter().enumerate() {
        if e.contains("x=0 y=2") {
            eprintln!("  {i:4}  {e}");
        }
    }
    eprintln!("\n=== ENC events touching (0,2) ===");
    for (i, e) in enc_events.iter().enumerate() {
        if e.contains("x=0 y=2") {
            eprintln!("  {i:4}  {e}");
        }
    }
}
