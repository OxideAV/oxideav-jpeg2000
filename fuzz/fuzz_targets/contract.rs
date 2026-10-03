#![no_main]

//! Fuzz target for the image-crate contract surface: `probe`, `info`,
//! `decode`, `decode_with` (limits, strict, reduce, layers) and the
//! `to_rgb8` / `to_rgba8` conversions on arbitrary bytes — a JP2 / JPH
//! file or a bare codestream, the sniff decides.
//!
//! Every call must return (`probe` infallibly, the rest as `Result`);
//! a successful `decode` must agree with `info` on geometry and layout,
//! validate, and convert to RGB(A) without panicking. The first byte
//! of the input scripts the options so the reduced / layer-limited /
//! strict paths get their share of the corpus.
//!
//! ## Bounds
//!
//! The limits keep an iteration from allocating attacker-scaled memory:
//! 1024 × 1024 pixels, a 64 MiB working set.

use libfuzzer_sys::fuzz_target;
use oxideav_jpeg2000::{decode, decode_with, info, probe, DecodeOptions};

fuzz_target!(|data: &[u8]| {
    let Some((&script, bytes)) = data.split_first() else {
        return;
    };
    let sniffed = probe(bytes);
    let info = info(bytes);
    if !sniffed {
        // Not a JPEG 2000 signature: the header walk must say so.
        assert!(info.is_err());
    }
    let opts = DecodeOptions::default()
        .with_max_width(1024)
        .with_max_height(1024)
        .with_max_bytes(64 << 20)
        .with_strict(script & 1 != 0)
        .with_reduce(if script & 2 != 0 { (script >> 4) & 3 } else { 0 })
        .with_layers(if script & 4 != 0 {
            Some(u16::from(script >> 5) + 1)
        } else {
            None
        });
    let img = if script & 8 != 0 {
        decode_with(bytes, &opts)
    } else {
        decode_with(bytes, &DecodeOptions::default().with_max_width(1024).with_max_height(1024).with_max_bytes(64 << 20))
    };
    if let Ok(img) = img {
        img.validate().expect("decoded image validates");
        if let Ok(i) = &info {
            assert_eq!(i.format, img.format);
            if opts.reduce == 0 || script & 8 == 0 {
                assert_eq!((i.width, i.height), (img.width, img.height));
            }
        }
        let rgb = img.to_rgb8();
        let rgba = img.to_rgba8();
        assert_eq!(rgb.len(), img.width as usize * img.height as usize * 3);
        assert_eq!(rgba.len(), img.width as usize * img.height as usize * 4);
    }
    // The limit-free default path on tiny inputs only (geometry cap).
    if bytes.len() < 512 {
        let _ = decode(bytes);
    }
});
