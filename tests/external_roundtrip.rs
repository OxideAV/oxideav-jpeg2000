//! End-to-end **external** round-trip:
//!
//! ```text
//!   oxideav encode  →  opj_decompress   →  PPM   →  opj_compress  →  oxideav decode
//!   (5/3 lossless)     (OpenJPEG CLI)              (OpenJPEG CLI)     (5/3 lossless)
//! ```
//!
//! On a deterministic random 640×480 RGB image. The 5/3 reversible
//! wavelet is bit-exact lossless, so round-tripping through OpenJPEG
//! at both transitions must reproduce the source RGB byte-for-byte.
//!
//! Workspace policy bars OpenJPEG source from the dep tree. The
//! `libloading` dev-dep is only used to `dlopen` libopenjp2 at startup
//! and confirm it is installed — the actual encode / decode is
//! performed by the `opj_compress` and `opj_decompress` CLI binaries
//! shipped alongside the library (workspace policy explicitly allows
//! "Binaries OK as black-box validators"). The reason for shelling out
//! instead of FFI-binding is the same as in `fuzz/src/lib.rs`:
//! `opj_cparameters_t` (~18 KB) and `opj_dparameters_t` (~8 KB) are too
//! brittle to mirror without the C header.
//!
//! The test silently skips (with an `eprintln!` note) on hosts that do
//! not have libopenjp2 + the CLI binaries available, so CI without
//! OpenJPEG installed stays green.

#![allow(unsafe_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use oxideav_core::CodecRegistry;
use oxideav_core::{
    CodecId, CodecParameters, Frame, Packet, PixelFormat, TimeBase, VideoFrame, VideoPlane,
};
use oxideav_jpeg2000::encode::{encode_frame, EncodeOptions};

mod openjpeg {
    //! Minimal subprocess shim mirroring the one in `fuzz/src/lib.rs`.
    //! Kept inline so the test crate doesn't need to depend on the
    //! fuzz crate (which has its own workspace).

    use libloading::Library;
    use std::process::{Command, Stdio};
    use std::sync::OnceLock;

    /// Conventional libopenjp2 shared-object names — covers macOS
    /// (`.dylib`), Linux (versioned + plain `.so`), and Windows.
    const CANDIDATES: &[&str] = &[
        "libopenjp2.dylib",
        "libopenjp2.7.dylib",
        "libopenjp2.so.7",
        "libopenjp2.so",
        "openjp2.dll",
    ];

    fn lib() -> Option<&'static Library> {
        static LIB: OnceLock<Option<Library>> = OnceLock::new();
        LIB.get_or_init(|| {
            for name in CANDIDATES {
                // SAFETY: `Library::new` is `unsafe` because loading a
                // shared object may run code at load time. libopenjp2
                // is well-behaved.
                if let Ok(l) = unsafe { Library::new(name) } {
                    return Some(l);
                }
            }
            None
        })
        .as_ref()
    }

    /// True iff libopenjp2 loads successfully **and** both
    /// `opj_compress` and `opj_decompress` are on PATH (the dlopen
    /// check is necessary but not sufficient — the `-tools` sub-package
    /// might be missing).
    pub fn available() -> bool {
        if lib().is_none() {
            return false;
        }
        cli_present("opj_compress") && cli_present("opj_decompress")
    }

    fn cli_present(bin: &str) -> bool {
        Command::new(bin)
            .arg("-h")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success() || s.code() == Some(1))
            .unwrap_or(false)
    }
}

/// Tiny self-contained LCG so the source image is fully deterministic
/// without pulling in a `rand` dev-dep. Numerical-Recipes constants
/// (Park-Miller-style multiplier).
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        // Avoid the zero state.
        Self(seed | 1)
    }
    fn next_u8(&mut self) -> u8 {
        // Standard LCG: x' = a*x + c (mod 2^64). High byte is well
        // mixed.
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 56) as u8
    }
}

const W: u32 = 640;
const H: u32 = 480;
const SEED: u64 = 0xC0FFEE_DEADBEEF;

/// Build a deterministic 640×480 packed RGB24 source image.
fn build_random_rgb() -> VideoFrame {
    let mut rng = Lcg::new(SEED);
    let mut data = Vec::with_capacity((W * H * 3) as usize);
    for _ in 0..(W * H) {
        data.push(rng.next_u8());
        data.push(rng.next_u8());
        data.push(rng.next_u8());
    }
    VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride: (W * 3) as usize,
            data,
        }],
    }
}

/// Decode a `.j2k` (or `.jp2`) byte string through the oxideav decoder
/// and reassemble the three component planes back into packed RGB24.
fn oxideav_decode_to_rgb(bytes: &[u8]) -> Vec<u8> {
    let mut reg = CodecRegistry::new();
    oxideav_jpeg2000::register_codecs(&mut reg);
    let params = CodecParameters::video(CodecId::new(oxideav_jpeg2000::CODEC_ID_STR));
    let mut dec = reg.make_decoder(&params).expect("factory");
    let pkt = Packet::new(0, TimeBase::new(1, 1), bytes.to_vec());
    dec.send_packet(&pkt).expect("send_packet");
    let vf = match dec.receive_frame().expect("receive_frame") {
        Frame::Video(v) => v,
        _ => panic!("expected video frame"),
    };
    assert_eq!(vf.planes.len(), 3, "expected 3 planes for RGB roundtrip");
    let w = W as usize;
    let h = H as usize;
    let mut packed = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            packed.push(vf.planes[0].data[y * vf.planes[0].stride + x]);
            packed.push(vf.planes[1].data[y * vf.planes[1].stride + x]);
            packed.push(vf.planes[2].data[y * vf.planes[2].stride + x]);
        }
    }
    packed
}

/// Per-process unique scratch directory for the test. Mirrors the
/// fuzz harness's `tempdir()` — `std::env::temp_dir()` is shared so
/// we synthesise a unique sub-path from PID + a counter rather than
/// pulling in a `tempfile` dev-dep.
fn scratch_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let p = std::env::temp_dir().join(format!("oxideav-jpeg2000-roundtrip-{pid}-{n}"));
    std::fs::create_dir_all(&p).expect("create scratch dir");
    p
}

/// Write a binary PPM (P6) file: ASCII header followed by raw RGB.
fn write_ppm_p6(path: &Path, rgb: &[u8], w: u32, h: u32) {
    assert_eq!(rgb.len(), (w as usize) * (h as usize) * 3);
    let mut f = std::fs::File::create(path).expect("create ppm");
    let header = format!("P6\n{w} {h}\n255\n");
    f.write_all(header.as_bytes()).expect("write ppm header");
    f.write_all(rgb).expect("write ppm body");
}

/// Parse a binary PPM (P6) file. Tolerates `#` comment lines in the
/// header. Returns `(width, height, packed_rgb)`.
fn parse_ppm_p6(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    assert_eq!(&bytes[0..2], b"P6", "expected P6 magic in PPM");
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
        assert_ne!(start, i, "empty PPM token");
        toks.push(String::from_utf8_lossy(&bytes[start..i]).into_owned());
    }
    // Skip the single whitespace terminator after maxval.
    assert!(i < bytes.len(), "truncated PPM");
    i += 1;
    let w: u32 = toks[0].parse().expect("PPM width");
    let h: u32 = toks[1].parse().expect("PPM height");
    let maxval: u32 = toks[2].parse().expect("PPM maxval");
    assert_eq!(maxval, 255, "expected 8-bit PPM (maxval=255)");
    let expected = (w as usize) * (h as usize) * 3;
    assert!(
        bytes.len() >= i + expected,
        "PPM body short ({} need, {} have)",
        expected,
        bytes.len() - i
    );
    (w, h, bytes[i..i + expected].to_vec())
}

/// Run `opj_decompress -i in.j2k -o out.ppm` and return the parsed
/// PPM body. Panics on failure (the test harness only enters this
/// path after `openjpeg::available()` returned true).
fn opj_decompress_to_rgb(j2k_bytes: &[u8], dir: &Path) -> Vec<u8> {
    let in_path = dir.join("opj_in.j2k");
    let out_path = dir.join("opj_out.ppm");
    std::fs::write(&in_path, j2k_bytes).expect("write j2k");
    let status = Command::new("opj_decompress")
        .arg("-i")
        .arg(&in_path)
        .arg("-o")
        .arg(&out_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn opj_decompress");
    assert!(status.success(), "opj_decompress failed: {status}");
    let raw = std::fs::read(&out_path).expect("read opj output ppm");
    let (w, h, rgb) = parse_ppm_p6(&raw);
    assert_eq!((w, h), (W, H), "opj_decompress geometry drift");
    rgb
}

/// Run `opj_compress -i in.ppm -o out.j2k -r 1` and return the
/// resulting raw `.j2k` codestream.
///
/// `-r 1` requests a single quality layer at compression ratio 1
/// (i.e. "lossless" rate). The reversible 5/3 wavelet is OpenJPEG's
/// default — `opj_compress -h` lists "Reversible DWT 5-3" under
/// "Default encoding options", and the irreversible 9/7 path is
/// **opt-in** via `-I`. We deliberately do NOT pass `-I` here: this
/// test exercises the lossless path, and `-I` would select the lossy
/// 9/7 transform and break the bit-exact round-trip.
fn opj_compress_to_j2k(rgb: &[u8], dir: &Path) -> Vec<u8> {
    let in_path = dir.join("opj_in.ppm");
    let out_path = dir.join("opj_out.j2k");
    write_ppm_p6(&in_path, rgb, W, H);
    let status = Command::new("opj_compress")
        .arg("-i")
        .arg(&in_path)
        .arg("-o")
        .arg(&out_path)
        // -r 1: one quality layer at ratio 1 (no truncation).
        // 5/3 reversible is the default; do NOT pass -I (which would
        // select the irreversible 9/7 path).
        .arg("-r")
        .arg("1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn opj_compress");
    assert!(status.success(), "opj_compress failed: {status}");
    std::fs::read(&out_path).expect("read opj j2k output")
}

#[test]
fn external_lossless_rgb_roundtrip_is_bit_exact() {
    if !openjpeg::available() {
        eprintln!(
            "external_lossless_rgb_roundtrip_is_bit_exact: \
             libopenjp2 / opj_compress / opj_decompress not available — skipping"
        );
        return;
    }

    let dir = scratch_dir();
    let src = build_random_rgb();
    let src_rgb = src.planes[0].data.clone();
    assert_eq!(src_rgb.len(), (W as usize) * (H as usize) * 3);

    // Step 1: oxideav encode (5/3 lossless, default num_decomp=5).
    let opts = EncodeOptions::default();
    let oxide_j2k = encode_frame(&Frame::Video(src.clone()), W, H, PixelFormat::Rgb24, &opts)
        .expect("oxideav encode");

    // Step 2: opj_decompress on our codestream → PPM → RGB.
    let opj_rgb = opj_decompress_to_rgb(&oxide_j2k, &dir);
    assert_eq!(
        opj_rgb, src_rgb,
        "OpenJPEG decode of our 5/3 lossless codestream must be bit-exact",
    );

    // Step 3: opj_compress that RGB back to a fresh J2K codestream.
    let opj_j2k = opj_compress_to_j2k(&opj_rgb, &dir);

    // Step 4: oxideav decode of OpenJPEG's codestream → RGB.
    let final_rgb = oxideav_decode_to_rgb(&opj_j2k);

    // Final assertion: the original RGB survived the full
    // encode → opj_decompress → opj_compress → decode round trip
    // bit-exactly. The 5/3 reversible wavelet guarantees this.
    assert_eq!(
        final_rgb.len(),
        src_rgb.len(),
        "RGB length mismatch after external round-trip"
    );
    let mismatches = final_rgb
        .iter()
        .zip(src_rgb.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(
        mismatches,
        0,
        "external lossless 5/3 RGB roundtrip must be bit-exact \
         (found {mismatches} byte mismatches out of {})",
        src_rgb.len()
    );

    // Best-effort scratch cleanup.
    let _ = std::fs::remove_dir_all(&dir);
}
