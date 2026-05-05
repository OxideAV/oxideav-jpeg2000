//! Encoder-side progression-order, POC, PPM and PPT tests
//! (T.800 §A.6.1 / §A.6.6 / §A.7.4 / §A.7.5).
//!
//! Coverage:
//!
//! - All five Part-1 progression orders (LRCP / RLCP / RPCL / PCRL /
//!   CPRL): encode then decode with our own decoder; the result must
//!   match the source bit-for-bit on the 5/3 reversible path.
//! - POC marker emission: encode with a non-empty POC schedule and
//!   verify the parser captures it and the round-trip image matches.
//! - PPM / PPT placement: encode with packed packet headers in the main
//!   header (PPM) or per tile-part (PPT) and verify the codestream
//!   parses + decodes correctly.
//! - `opj_decompress` cross-decode: each emitted codestream must also
//!   decode through OpenJPEG when the binary is available on PATH (the
//!   test is skipped otherwise).

use std::path::PathBuf;
use std::process::Command;

use oxideav_core::CodecRegistry;
use oxideav_core::{
    CodecId, CodecParameters, Frame, Packet, PixelFormat, TimeBase, VideoFrame, VideoPlane,
};
use oxideav_jpeg2000::codestream;
use oxideav_jpeg2000::decode::tile::PocProgression;
use oxideav_jpeg2000::encode::{
    encode_frame, EncodeOptions, PacketHeaderPlacement, ProgressionOrder,
};

/// Build an 8-bit Gray gradient.
fn build_gray_gradient(w: u32, h: u32) -> VideoFrame {
    let mut data = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let v = ((x + y) * 255 / (w + h - 2)).min(255) as u8;
            data.push(v);
        }
    }
    VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride: w as usize,
            data,
        }],
    }
}

/// Build a small RGB pattern with each channel taking a distinct slope.
fn build_rgb_pattern(w: u32, h: u32) -> VideoFrame {
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let r = (x * 255 / w.max(1)) as u8;
            let g = (y * 255 / h.max(1)) as u8;
            let b = ((x + y) * 255 / (w + h).max(1)) as u8;
            data.push(r);
            data.push(g);
            data.push(b);
        }
    }
    VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride: (w * 3) as usize,
            data,
        }],
    }
}

fn decode_with_us(bytes: &[u8]) -> VideoFrame {
    let mut reg = CodecRegistry::new();
    oxideav_jpeg2000::register_codecs(&mut reg);
    let params = CodecParameters::video(CodecId::new(oxideav_jpeg2000::CODEC_ID_STR));
    let mut dec = reg.make_decoder(&params).expect("decoder factory");
    let pkt = Packet::new(0, TimeBase::new(1, 1), bytes.to_vec());
    dec.send_packet(&pkt).expect("send_packet");
    match dec.receive_frame().expect("receive_frame") {
        Frame::Video(v) => v,
        _ => panic!("expected video frame"),
    }
}

/// True if `bin` is invocable.
fn tool_available(bin: &str) -> bool {
    Command::new(bin)
        .arg("-h")
        .output()
        .map(|o| o.status.success() || !o.stdout.is_empty() || !o.stderr.is_empty())
        .unwrap_or(false)
}

fn tmp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "oxideav-jpeg2000-encode-progression-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p.push(name);
    p
}

/// Run `opj_decompress` to convert the encoded bytes into a `.pgm` (gray)
/// or `.ppm` (RGB) and return the parsed pixel buffer. Returns `None`
/// when `opj_decompress` is not on PATH so the caller can skip.
///
/// When `opj_decompress` IS on PATH the call is required to succeed —
/// any failure propagates as a panic so we don't silently miss
/// regressions in encoder output.
fn opj_decompress_to_planes(j2k_bytes: &[u8], rgb: bool, label: &str) -> Option<Vec<u8>> {
    if !tool_available("opj_decompress") {
        return None;
    }
    let suffix = if rgb { "ppm" } else { "pgm" };
    let in_path = tmp_path(&format!("in-{}.j2k", rand_token()));
    let out_path = tmp_path(&format!("out-{}.{}", rand_token(), suffix));
    std::fs::write(&in_path, j2k_bytes).expect("write j2k");
    let status = Command::new("opj_decompress")
        .arg("-i")
        .arg(&in_path)
        .arg("-o")
        .arg(&out_path)
        .output()
        .expect("spawn opj_decompress");
    let _ = std::fs::remove_file(&in_path);
    if !status.status.success() {
        let _ = std::fs::remove_file(&out_path);
        panic!(
            "{label}: opj_decompress refused our codestream: stdout={} stderr={}",
            String::from_utf8_lossy(&status.stdout),
            String::from_utf8_lossy(&status.stderr)
        );
    }
    let buf = std::fs::read(&out_path).unwrap_or_else(|e| panic!("{label}: read opj output: {e}"));
    let _ = std::fs::remove_file(&out_path);
    let off =
        pnm_data_offset(&buf).unwrap_or_else(|| panic!("{label}: opj output has no PNM header"));
    Some(buf[off..].to_vec())
}

fn rand_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{n}")
}

/// Skip past the PNM (P5/P6) header into the raw byte plane.
fn pnm_data_offset(buf: &[u8]) -> Option<usize> {
    if buf.len() < 4 {
        return None;
    }
    if &buf[..2] != b"P5" && &buf[..2] != b"P6" {
        return None;
    }
    let mut i = 2usize;
    let mut tokens = 0;
    while i < buf.len() {
        while i < buf.len() && matches!(buf[i], b' ' | b'\t' | b'\n' | b'\r') {
            i += 1;
        }
        if i < buf.len() && buf[i] == b'#' {
            while i < buf.len() && buf[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        while i < buf.len() && !matches!(buf[i], b' ' | b'\t' | b'\n' | b'\r') {
            i += 1;
        }
        tokens += 1;
        if tokens == 3 {
            if i < buf.len() {
                return Some(i + 1);
            }
            return None;
        }
    }
    None
}

/// Round-trip a Gray image through the encoder under a chosen
/// progression order and verify our decoder reproduces it bit-exact.
fn assert_progression_round_trip(progression: ProgressionOrder, label: &str) {
    let src = build_gray_gradient(64, 64);
    let opts = EncodeOptions {
        progression,
        ..EncodeOptions::default()
    };
    let bytes = encode_frame(
        &Frame::Video(src.clone()),
        64,
        64,
        PixelFormat::Gray8,
        &opts,
    )
    .expect("encode");

    // The COD's progression byte must reflect the chosen order.
    let cs = codestream::parse(&bytes).expect("parse encoded");
    let cod_payload = cs.cod.as_ref().expect("COD present");
    assert_eq!(
        cod_payload[1], progression as u8,
        "{label}: COD progression byte must equal requested order"
    );

    let decoded = decode_with_us(&bytes);
    assert_eq!(
        decoded.planes[0].data, src.planes[0].data,
        "{label}: bit-exact round-trip"
    );

    if let Some(via_opj) = opj_decompress_to_planes(&bytes, false, label) {
        assert_eq!(
            via_opj, src.planes[0].data,
            "{label}: opj_decompress must match source"
        );
    }
}

#[test]
fn encoder_lrcp_round_trip_bit_exact() {
    assert_progression_round_trip(ProgressionOrder::Lrcp, "LRCP");
}

#[test]
fn encoder_rlcp_round_trip_bit_exact() {
    assert_progression_round_trip(ProgressionOrder::Rlcp, "RLCP");
}

#[test]
fn encoder_rpcl_round_trip_bit_exact() {
    assert_progression_round_trip(ProgressionOrder::Rpcl, "RPCL");
}

#[test]
fn encoder_pcrl_round_trip_bit_exact() {
    assert_progression_round_trip(ProgressionOrder::Pcrl, "PCRL");
}

#[test]
fn encoder_cprl_round_trip_bit_exact() {
    assert_progression_round_trip(ProgressionOrder::Cprl, "CPRL");
}

/// Pull per-channel R/G/B plane buffers out of an interleaved Rgb24
/// source frame so we can compare against the decoder's planar output.
fn rgb24_to_planar(src: &VideoFrame, w: usize, h: usize) -> [Vec<u8>; 3] {
    let stride = src.planes[0].stride;
    let mut r = Vec::with_capacity(w * h);
    let mut g = Vec::with_capacity(w * h);
    let mut b = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let off = y * stride + 3 * x;
            r.push(src.planes[0].data[off]);
            g.push(src.planes[0].data[off + 1]);
            b.push(src.planes[0].data[off + 2]);
        }
    }
    [r, g, b]
}

/// PCRL on a 3-channel RGB source: component-outer order produces a
/// different on-the-wire packet ordering than LRCP, but the decoded
/// image is unchanged. We compare per-component planes (the decoder
/// emits a planar Yuv444P-shaped frame for 3-component streams) so
/// disabling MCT keeps the round-trip bit-exact.
#[test]
fn encoder_pcrl_rgb_round_trip_bit_exact() {
    let src = build_rgb_pattern(48, 48);
    let opts = EncodeOptions {
        progression: ProgressionOrder::Pcrl,
        // Disable RCT so the round-trip is bit-exact against the raw
        // RGB input — when RCT is on the frame round-trips through the
        // YCbCr representation and per-channel comparison no longer
        // applies (the decoder reports Yuv444P planes pre-RCT, which
        // are not the original R/G/B bytes).
        use_color_transform: false,
        ..EncodeOptions::default()
    };
    let bytes = encode_frame(
        &Frame::Video(src.clone()),
        48,
        48,
        PixelFormat::Rgb24,
        &opts,
    )
    .expect("encode");
    let decoded = decode_with_us(&bytes);
    assert_eq!(decoded.planes.len(), 3, "three planes for RGB-no-MCT");
    let expected = rgb24_to_planar(&src, 48, 48);
    for (i, exp) in expected.iter().enumerate() {
        assert_eq!(
            &decoded.planes[i].data, exp,
            "RGB PCRL plane {i} bit-exact round-trip"
        );
    }
}

/// Build a low-chroma RGB pattern: the three channels stay close in
/// value so the forward RCT chroma `Cb = B - G` and `Cr = R - G` fit
/// inside the 8-bit signed range expressible by the standard SIZ
/// `Ssiz` byte (the encoder caps unsigned 8-bit precision and clips
/// outside this range — see `lib.rs` known-limitations note).
fn build_rgb_low_chroma(w: u32, h: u32) -> VideoFrame {
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            // Diagonal gray with a tiny per-channel offset so the
            // channels are not identical. Max chroma magnitude is 4.
            let v = ((x + y) * 255 / (w + h - 2)).min(255) as i32;
            let r = v.clamp(0, 255) as u8;
            let g = (v + 2).clamp(0, 255) as u8;
            let b = (v + 4).clamp(0, 255) as u8;
            data.push(r);
            data.push(g);
            data.push(b);
        }
    }
    VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride: (w * 3) as usize,
            data,
        }],
    }
}

/// CPRL with RCT enabled: 5/3 + RCT is mathematically lossless on 8-bit
/// RGB *when the chroma excursions fit in the signed 8-bit range*. The
/// encoder still stores Cb / Cr in unsigned 8-bit components (Ssiz with
/// no sign bit), so any |Cb| > 128 would clip; we use a low-chroma
/// pattern to stay inside that envelope (see lib.rs known-limitations).
#[test]
fn encoder_cprl_rgb_with_rct_round_trip_bit_exact() {
    let src = build_rgb_low_chroma(48, 48);
    let opts = EncodeOptions {
        progression: ProgressionOrder::Cprl,
        use_color_transform: true,
        ..EncodeOptions::default()
    };
    let bytes = encode_frame(
        &Frame::Video(src.clone()),
        48,
        48,
        PixelFormat::Rgb24,
        &opts,
    )
    .expect("encode");
    let decoded = decode_with_us(&bytes);
    // Three planes after the decoder's inverse RCT (T.800 §G.1).
    assert_eq!(decoded.planes.len(), 3);
    let expected = rgb24_to_planar(&src, 48, 48);
    for (i, exp) in expected.iter().enumerate() {
        assert_eq!(
            &decoded.planes[i].data, exp,
            "RGB CPRL+RCT plane {i} bit-exact round-trip"
        );
    }
}

// -- POC marker -----------------------------------------------------------

/// Emit a POC marker that wraps a single identity progression-order
/// volume. Verifies the encoder serialises POC correctly and our decoder
/// honours it (the round-trip image is unchanged).
#[test]
fn encoder_poc_identity_lrcp_decodes_bit_exactly() {
    let src = build_gray_gradient(64, 64);
    let opts = EncodeOptions {
        progression: ProgressionOrder::Lrcp,
        poc: vec![PocProgression {
            res_start: 0,
            comp_start: 0,
            layer_end: 1,
            res_end: 6, // num_decomp + 1 = 6 with the default 5 decomp levels
            comp_end: 1,
            progression: 0, // LRCP
        }],
        ..EncodeOptions::default()
    };
    let bytes = encode_frame(
        &Frame::Video(src.clone()),
        64,
        64,
        PixelFormat::Gray8,
        &opts,
    )
    .expect("encode");
    let cs = codestream::parse(&bytes).expect("parse encoded");
    let poc = cs.poc.as_ref().expect("POC marker present");
    // 7 bytes per progression (Csiz < 257) — single volume.
    assert_eq!(poc.len(), 7, "single-volume POC payload must be 7 bytes");
    let decoded = decode_with_us(&bytes);
    assert_eq!(
        decoded.planes[0].data, src.planes[0].data,
        "POC LRCP identity must round-trip bit-exact"
    );
}

/// POC marker that switches to PCRL via the schedule: the COD says LRCP
/// but the POC volume overrides it. The packet emit loop honours the
/// POC volume's `Ppoc`; our decoder + opj_decompress should agree.
#[test]
fn encoder_poc_pcrl_volume_decodes_bit_exactly() {
    let src = build_rgb_pattern(48, 48);
    let opts = EncodeOptions {
        progression: ProgressionOrder::Lrcp,
        // RCT off so 5/3 round-trips bit-exact against raw RGB.
        use_color_transform: false,
        poc: vec![PocProgression {
            res_start: 0,
            comp_start: 0,
            layer_end: 1,
            res_end: 6,
            comp_end: 3,
            progression: 3, // PCRL
        }],
        ..EncodeOptions::default()
    };
    let bytes = encode_frame(
        &Frame::Video(src.clone()),
        48,
        48,
        PixelFormat::Rgb24,
        &opts,
    )
    .expect("encode");
    let cs = codestream::parse(&bytes).expect("parse encoded");
    assert!(cs.poc.is_some(), "POC marker must be present");
    let decoded = decode_with_us(&bytes);
    assert_eq!(decoded.planes.len(), 3, "RGB-no-MCT yields 3 planes");
    let expected = rgb24_to_planar(&src, 48, 48);
    for (i, exp) in expected.iter().enumerate() {
        assert_eq!(
            &decoded.planes[i].data, exp,
            "POC PCRL volume plane {i} bit-exact"
        );
    }

    if let Some(via_opj) = opj_decompress_to_planes(&bytes, true, "POC-PCRL") {
        // PPM/PCRL produce interleaved RGB. The source is also
        // interleaved Rgb24, so the byte plane comparison applies
        // directly.
        assert_eq!(
            via_opj, src.planes[0].data,
            "opj_decompress must accept POC PCRL volume"
        );
    }
}

// -- PPM / PPT placement --------------------------------------------------

/// Encode with packet headers packed into a main-header PPM segment.
/// Verifies the codestream parses, the PPM payload is captured, and the
/// decoded image matches the source.
#[test]
fn encoder_ppm_main_header_round_trip_bit_exact() {
    let src = build_gray_gradient(64, 64);
    let opts = EncodeOptions {
        packet_header_placement: PacketHeaderPlacement::PackedMainHeader,
        ..EncodeOptions::default()
    };
    let bytes = encode_frame(
        &Frame::Video(src.clone()),
        64,
        64,
        PixelFormat::Gray8,
        &opts,
    )
    .expect("encode");
    let cs = codestream::parse(&bytes).expect("parse encoded");
    assert_eq!(cs.ppm.len(), 1, "exactly one PPM segment in main header");
    for tp in &cs.tile_parts {
        assert!(
            tp.ppt.is_empty(),
            "PPT must be empty when PPM is in main header"
        );
    }
    let decoded = decode_with_us(&bytes);
    assert_eq!(
        decoded.planes[0].data, src.planes[0].data,
        "PPM round-trip must be bit-exact"
    );

    if let Some(via_opj) = opj_decompress_to_planes(&bytes, false, "PPM") {
        assert_eq!(
            via_opj, src.planes[0].data,
            "opj_decompress must accept PPM stream"
        );
    }
}

/// Encode with packet headers packed per tile-part (PPT). Verifies the
/// codestream parses, the PPT payload is captured on each tile-part,
/// and the decoded image matches.
#[test]
fn encoder_ppt_per_tile_part_round_trip_bit_exact() {
    let src = build_gray_gradient(64, 64);
    let opts = EncodeOptions {
        packet_header_placement: PacketHeaderPlacement::PackedPerTilePart,
        ..EncodeOptions::default()
    };
    let bytes = encode_frame(
        &Frame::Video(src.clone()),
        64,
        64,
        PixelFormat::Gray8,
        &opts,
    )
    .expect("encode");
    let cs = codestream::parse(&bytes).expect("parse encoded");
    assert!(cs.ppm.is_empty(), "PPM must be empty for PPT mode");
    for tp in &cs.tile_parts {
        assert_eq!(tp.ppt.len(), 1, "each tile-part carries one PPT segment");
    }
    let decoded = decode_with_us(&bytes);
    assert_eq!(
        decoded.planes[0].data, src.planes[0].data,
        "PPT round-trip must be bit-exact"
    );

    if let Some(via_opj) = opj_decompress_to_planes(&bytes, false, "PPT") {
        assert_eq!(
            via_opj, src.planes[0].data,
            "opj_decompress must accept PPT stream"
        );
    }
}

/// PPT on the 9/7 lossy path: bit-exactness against the source isn't
/// guaranteed (lossy quantisation), but the decoded PSNR vs source must
/// stay above 30 dB and the stream must parse.
#[test]
fn encoder_ppt_lossy_decodes_above_30db() {
    use oxideav_jpeg2000::encode::TransformMode;
    let src = build_gray_gradient(64, 64);
    let opts = EncodeOptions {
        transform: TransformMode::Irreversible97,
        packet_header_placement: PacketHeaderPlacement::PackedPerTilePart,
        ..EncodeOptions::default()
    };
    let bytes = encode_frame(
        &Frame::Video(src.clone()),
        64,
        64,
        PixelFormat::Gray8,
        &opts,
    )
    .expect("encode");
    let cs = codestream::parse(&bytes).expect("parse encoded");
    assert_eq!(cs.tile_parts.len(), 1);
    assert_eq!(cs.tile_parts[0].ppt.len(), 1);
    let decoded = decode_with_us(&bytes);
    let psnr = psnr_u8(&decoded.planes[0].data, &src.planes[0].data);
    assert!(
        psnr > 30.0,
        "PPT 9/7 round-trip PSNR must exceed 30 dB (got {psnr:.2})"
    );
}

fn psnr_u8(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let mut sse: u64 = 0;
    for (&x, &y) in a.iter().zip(b.iter()) {
        let d = x as i32 - y as i32;
        sse += (d * d) as u64;
    }
    if sse == 0 {
        return f64::INFINITY;
    }
    let mse = sse as f64 / a.len() as f64;
    10.0 * (255.0 * 255.0 / mse).log10()
}

// -- ProgressionOrder string parsing --------------------------------------

#[test]
fn progression_order_from_short_str() {
    assert_eq!(
        ProgressionOrder::from_short_str("lrcp"),
        Some(ProgressionOrder::Lrcp)
    );
    assert_eq!(
        ProgressionOrder::from_short_str("RLCP"),
        Some(ProgressionOrder::Rlcp)
    );
    assert_eq!(
        ProgressionOrder::from_short_str("rpcl"),
        Some(ProgressionOrder::Rpcl)
    );
    assert_eq!(
        ProgressionOrder::from_short_str("PCRL"),
        Some(ProgressionOrder::Pcrl)
    );
    assert_eq!(
        ProgressionOrder::from_short_str("cprl"),
        Some(ProgressionOrder::Cprl)
    );
    assert_eq!(ProgressionOrder::from_short_str("xxxx"), None);
}
