//! Runtime libopenjp2 interop for the cross-decode fuzz harnesses.
//!
//! Availability is probed via `dlopen` of libopenjp2 at first call, so
//! there is no `openjpeg-sys`-style build-script dep that would pull
//! OpenJPEG source into the workspace's cargo dep tree (workspace
//! policy bars external library code as reference). Each harness checks
//! [`openjpeg::available`] up front and `return`s early when the shared
//! library isn't installed, so fuzz binaries built on a host without
//! OpenJPEG simply do nothing instead of panicking.
//!
//! The actual encode / decode is performed by the `opj_compress` and
//! `opj_decompress` CLI binaries shipped in the same Debian package
//! (`libopenjp2-tools`). The dlopen check confirms that libopenjp2 is
//! installed (the CLIs link against it dynamically, so their presence
//! follows the library's). Workspace policy explicitly allows
//! "Binaries OK as black-box validators".
//!
//! The decision to subprocess rather than direct-FFI bind is driven by
//! the size and version-fragility of `opj_cparameters_t` (~18 KB) and
//! `opj_dparameters_t` (~8 KB) — replicating those structs in Rust
//! without the C header would be brittle, while subprocessing keeps
//! the harness portable across libopenjp2 versions.
//!
//! Install OpenJPEG with `brew install openjpeg` (macOS) or
//! `apt install libopenjp2-tools libopenjp2-7-dev` (Debian/Ubuntu).
//! The loader probes the conventional shared-object names for both
//! platforms.

#![allow(unsafe_code)]

pub mod openjpeg {
    use libloading::Library;
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::sync::OnceLock;

    /// Conventional libopenjp2 shared-object names the loader will try
    /// in order. Covers macOS (`.dylib`), Linux (versioned + plain
    /// `.so`), and Windows (`.dll`).
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
                // SAFETY: `Library::new` is documented as unsafe because
                // the loaded library may run code at load time. We
                // accept that risk for fuzz tooling — libopenjp2 is a
                // well-behaved shared library.
                if let Ok(l) = unsafe { Library::new(name) } {
                    return Some(l);
                }
            }
            None
        })
        .as_ref()
    }

    /// True iff a libopenjp2 shared library was successfully loaded
    /// **and** the matching CLI binaries `opj_compress` and
    /// `opj_decompress` are on `PATH`. Cross-decode fuzz harnesses
    /// early-return when this is false so the binary still runs without
    /// an oracle (the assertions just don't fire).
    pub fn available() -> bool {
        if lib().is_none() {
            return false;
        }
        // Confirm the CLI binaries are present too — `dlopen` of the
        // library is necessary (proves the package is installed) but
        // not sufficient (the `-tools` sub-package may not be there).
        Command::new("opj_compress")
            .arg("-h")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success() || s.code() == Some(1))
            .unwrap_or(false)
            && Command::new("opj_decompress")
                .arg("-h")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success() || s.code() == Some(1))
                .unwrap_or(false)
    }

    /// An RGB image as decoded by OpenJPEG, normalised to interleaved
    /// 8-bit RGB.
    pub struct DecodedRgb {
        pub width: u32,
        pub height: u32,
        /// Tightly packed RGB, length `width * height * 3`.
        pub rgb: Vec<u8>,
    }

    /// Encode an interleaved 8-bit RGB image to a `.j2k` codestream
    /// (raw, no JP2 wrapper) using OpenJPEG's lossless 5/3 path. The
    /// input is staged through a temporary PPM (P6) file because
    /// `opj_compress` only reads from disk. Returns `None` on encode
    /// failure (e.g. OpenJPEG rejects degenerate parameters).
    pub fn encode_lossless_rgb(rgb: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
        if (rgb.len() as u64) != (width as u64) * (height as u64) * 3 {
            return None;
        }
        let dir = tempdir()?;
        let in_path = dir.join("in.ppm");
        let out_path = dir.join("out.j2k");

        // Write a binary PPM (P6) header followed by the pixel bytes.
        let mut f = std::fs::File::create(&in_path).ok()?;
        let header = format!("P6\n{width} {height}\n255\n");
        f.write_all(header.as_bytes()).ok()?;
        f.write_all(rgb).ok()?;
        drop(f);

        // `-r 1` requests a single quality layer at compression ratio 1
        // and combined with the default reversible 5/3 wavelet yields a
        // lossless codestream. We override the default 6-resolution
        // pyramid (`-n 1`) because tiny fuzz inputs (1×N or N×1) cannot
        // support the default decomposition depth and OpenJPEG would
        // reject them. One DWT level keeps things general.
        let status = Command::new("opj_compress")
            .arg("-i")
            .arg(&in_path)
            .arg("-o")
            .arg(&out_path)
            .arg("-r")
            .arg("1")
            .arg("-n")
            .arg("1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        if !status.success() {
            cleanup(&dir);
            return None;
        }
        let bytes = std::fs::read(&out_path).ok();
        cleanup(&dir);
        bytes
    }

    /// Decode a `.j2k` (or `.jp2`) byte string to interleaved 8-bit
    /// RGB via `opj_decompress` → PPM. Returns `None` on header parse
    /// failure, decode failure, or non-3-component output (we only
    /// validate the RGB path here).
    pub fn decode_to_rgb(data: &[u8]) -> Option<DecodedRgb> {
        let dir = tempdir()?;
        // Pick the file extension from the magic so opj_decompress's
        // format auto-detect succeeds. JP2 starts with the 12-byte
        // signature box; raw J2K starts with FF 4F (SOC).
        let ext = if data.len() >= 12
            && data[..12]
                == [
                    0x00, 0x00, 0x00, 0x0C, b'j', b'P', b' ', b' ', 0x0D, 0x0A, 0x87, 0x0A,
                ] {
            "jp2"
        } else if data.len() >= 2 && data[0] == 0xFF && data[1] == 0x4F {
            "j2k"
        } else {
            cleanup(&dir);
            return None;
        };
        let in_path = dir.join(format!("in.{ext}"));
        let out_path = dir.join("out.ppm");
        std::fs::write(&in_path, data).ok()?;

        let status = Command::new("opj_decompress")
            .arg("-i")
            .arg(&in_path)
            .arg("-o")
            .arg(&out_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        if !status.success() {
            cleanup(&dir);
            return None;
        }

        let raw = std::fs::read(&out_path).ok();
        cleanup(&dir);
        let raw = raw?;
        let parsed = parse_ppm_p6(&raw)?;
        Some(parsed)
    }

    /// Parse a binary PPM (P6) file into a `DecodedRgb`. Tolerates `#`
    /// comment lines in the header. Returns `None` on malformed input
    /// or non-255 maxval (we don't try to scale).
    fn parse_ppm_p6(bytes: &[u8]) -> Option<DecodedRgb> {
        if bytes.len() < 2 || &bytes[0..2] != b"P6" {
            return None;
        }
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
            if start == i {
                return None;
            }
            toks.push(String::from_utf8_lossy(&bytes[start..i]).into_owned());
        }
        if i >= bytes.len() {
            return None;
        }
        // Skip the single whitespace terminator after maxval.
        i += 1;
        let w: u32 = toks[0].parse().ok()?;
        let h: u32 = toks[1].parse().ok()?;
        let maxval: u32 = toks[2].parse().ok()?;
        if maxval != 255 {
            return None;
        }
        let expected = (w as usize).checked_mul(h as usize)?.checked_mul(3)?;
        if bytes.len() < i + expected {
            return None;
        }
        Some(DecodedRgb {
            width: w,
            height: h,
            rgb: bytes[i..i + expected].to_vec(),
        })
    }

    fn tempdir() -> Option<std::path::PathBuf> {
        // We can't depend on `tempfile` (would bloat the fuzz dep tree)
        // and `std::env::temp_dir()` is shared, so synthesise a
        // unique-per-process subdirectory using PID + a monotonic
        // counter. Cleaned up by `cleanup` after use.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let p = std::env::temp_dir().join(format!("oxideav-jpeg2000-fuzz-{pid}-{n}"));
        std::fs::create_dir_all(&p).ok()?;
        Some(p)
    }

    fn cleanup(dir: &std::path::Path) {
        let _ = std::fs::remove_dir_all(dir);
    }
}
