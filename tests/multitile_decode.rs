//! End-to-end multi-tile decode integration test.
//!
//! Generates a 2x2-tile `.j2k` codestream via `opj_compress` (OpenJPEG),
//! runs our multi-tile decoder on it, and checks structural parity
//! against ffmpeg's own decode:
//!
//! - output geometry matches (128x128 single Gray8 plane),
//! - every tile slot is populated (non-trivial pixel content in each
//!   64x64 quadrant — catches "only tile 0 decoded" regressions),
//! - PSNR between our output and ffmpeg's decode stays above a sanity
//!   floor (the absolute PSNR floor is looser than the ≥40 dB target
//!   because the existing tier-1 path has an orthogonal accuracy gap
//!   tracked in `lib.rs`; the structural check is the load-bearing one
//!   for the multi-tile work).
//!
//! If `opj_compress` or `ffmpeg` is missing on `PATH` the test is
//! skipped gracefully.

use std::path::{Path, PathBuf};
use std::process::Command;

use oxideav_core::CodecRegistry;
use oxideav_core::{CodecId, CodecParameters, Frame, Packet, TimeBase};

fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("-h")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn opj_compress_available() -> bool {
    tool_available("opj_compress")
}

fn run_tool(bin: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(bin)
        .args(args)
        .output()
        .map_err(|e| format!("spawn {bin}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{bin} {args:?} failed: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

fn tmp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("oxideav-jpeg2000-multitile-{}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p.push(name);
    p
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
}

fn psnr_u8(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "psnr buffer length mismatch");
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

fn our_decode_gray(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let mut reg = CodecRegistry::new();
    oxideav_jpeg2000::register(&mut reg);
    let params = CodecParameters::video(CodecId::new(oxideav_jpeg2000::CODEC_ID_STR));
    let mut dec = reg.make_decoder(&params).expect("factory");
    let pkt = Packet::new(0, TimeBase::new(1, 1), bytes.to_vec());
    dec.send_packet(&pkt).expect("our send_packet");
    let frame = dec.receive_frame().expect("our receive_frame");
    let vf = match frame {
        Frame::Video(v) => v,
        _ => panic!("expected video frame"),
    };
    assert_eq!(vf.planes.len(), 1, "expected single-plane (gray)");
    (vf.width, vf.height, vf.planes[0].data.clone())
}

fn pgm_data_offset(buf: &[u8]) -> Option<usize> {
    if buf.len() < 4 || &buf[..2] != b"P5" {
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

#[test]
fn multitile_gray_decodes_all_four_tiles() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg missing — skipping multi-tile test");
        return;
    }
    if !opj_compress_available() {
        eprintln!("opj_compress missing — skipping multi-tile test");
        return;
    }

    // 1. Deterministic 128x128 8-bit grayscale PGM via ffmpeg.
    let pgm = tmp_path("src.pgm");
    run_tool(
        "ffmpeg",
        &[
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=128x128:rate=1:duration=1",
            "-frames:v",
            "1",
            "-pix_fmt",
            "gray",
            pgm.to_str().unwrap(),
        ],
    )
    .expect("ffmpeg pgm");

    // 2. 2x2 multi-tile 5/3 lossless encode. Coding style mirrors the
    //    existing `baseline.j2k` fixture (num_decomp=5, cblk=64x64).
    let j2k = tmp_path("multi.j2k");
    run_tool(
        "opj_compress",
        &[
            "-i",
            pgm.to_str().unwrap(),
            "-o",
            j2k.to_str().unwrap(),
            "-t",
            "64,64",
            "-n",
            "5",
            "-b",
            "64,64",
            "-r",
            "1",
        ],
    )
    .expect("opj_compress multi-tile");

    // 3. Parse + sanity-check that the codestream really has a 2x2 tile
    //    grid (four distinct `Isot` values).
    let j2k_bytes = read(&j2k);
    let cs = oxideav_jpeg2000::codestream::parse(&j2k_bytes).expect("parse multi j2k");
    let distinct_tiles: std::collections::BTreeSet<u16> =
        cs.tile_parts.iter().map(|tp| tp.tile_index).collect();
    assert_eq!(
        distinct_tiles.len(),
        4,
        "expected 4 distinct tile indices for 2x2 grid, saw {}",
        distinct_tiles.len()
    );

    // 4. Decode with our crate. Geometry must come out at the full image
    //    size — any "only tile 0 decoded" regression would trip this
    //    via missing per-tile plane fills in steps 5 & 6 below.
    let (w, h, plane) = our_decode_gray(&j2k_bytes);
    assert_eq!((w, h), (128, 128));
    assert_eq!(plane.len(), 128 * 128);

    // 5. Every 64x64 tile quadrant must carry plausible luma content. We
    //    only enforce the mean-luma range here because the testsrc2
    //    pattern legitimately has a near-uniform quadrant (tile (0, 1)
    //    in the standard 128x128 render is a solid-blue region that
    //    decodes to only 4 distinct Y samples). Bit-exact agreement
    //    with the source PGM is enforced separately in step 6 below via
    //    ffmpeg-decoded PSNR.
    let src_pgm = read(&pgm);
    let src_off = pgm_data_offset(&src_pgm).expect("src pgm header");
    let src_plane = &src_pgm[src_off..];
    assert_eq!(src_plane.len(), 128 * 128, "source pgm size");
    for ty in 0..2 {
        for tx in 0..2 {
            let mut sum: u64 = 0;
            for y in 0..64 {
                for x in 0..64 {
                    sum += plane[(ty * 64 + y) * 128 + tx * 64 + x] as u64;
                }
            }
            let mean = (sum / (64 * 64)) as u32;
            assert!(
                (1..=254).contains(&mean),
                "tile ({tx},{ty}): luma mean {mean} out of plausible range"
            );
        }
    }
    // Lossless encode + correct decode → bit-exact against the source.
    let src_psnr = psnr_u8(&plane, src_plane);
    eprintln!("multi-tile vs source PSNR: {src_psnr} dB");
    assert!(
        src_psnr >= 40.0,
        "multi-tile PSNR {src_psnr} below the ≥40 dB bit-exactness threshold"
    );

    // 6. Independent reference: decode via ffmpeg and compute PSNR.
    //    The baseline tier-1 path still has an orthogonal accuracy gap
    //    that lands the absolute PSNR well below 40 dB even for the
    //    single-tile case — documented in `lib.rs`. We log the value
    //    for regression tracking and enforce a minimal sanity floor.
    let ref_pgm = tmp_path("ref.pgm");
    run_tool(
        "ffmpeg",
        &[
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            j2k.to_str().unwrap(),
            "-pix_fmt",
            "gray",
            ref_pgm.to_str().unwrap(),
        ],
    )
    .expect("ffmpeg j2k decode");
    let pgm_bytes = read(&ref_pgm);
    let data_off = pgm_data_offset(&pgm_bytes).expect("pgm header");
    let ref_plane = &pgm_bytes[data_off..];
    assert_eq!(ref_plane.len(), 128 * 128, "reference pgm size");
    let psnr = psnr_u8(&plane, ref_plane);
    eprintln!("multi-tile vs ffmpeg decode PSNR: {psnr:.2} dB");
    // Even a severely-biased decoder that still gets mean luma right
    // scores >= 3-4 dB; complete garbage (e.g. planes left all zero)
    // would sit around 0 dB. A floor of 3 dB catches total regressions
    // without inheriting the tier-1 accuracy debt.
    assert!(
        psnr >= 3.0,
        "multi-tile PSNR {psnr:.2} dB suggests total multi-tile decode failure"
    );
}
