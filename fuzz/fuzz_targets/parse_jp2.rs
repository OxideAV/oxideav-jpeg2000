#![no_main]

//! Panic-free fuzz target for the JP2 ISO BMFF box-wrapper parser
//! reached through [`oxideav_jpeg2000::jp2::parse_jp2`].
//!
//! Exercises the T.800 Annex I file-format surface:
//!
//! * §I.4 + Figure I.4 / Table I.1 binary box layout, including all
//!   three length encodings — standard `LBox`, extended `LBox = 1` +
//!   `XLBox` (8-byte length), and "until end of file" `LBox = 0`.
//! * §I.5.1 Signature box (`jP  `).
//! * §I.5.2 + Tables I.3 / I.4 File Type box (`ftyp` — brand, minor
//!   version, compatibility list).
//! * §I.5.3 + Figure I.7 JP2 Header superbox (`jp2h`):
//!   * §I.5.3.1 + Tables I.5 / I.6 Image Header box (`ihdr`).
//!   * §I.5.3.2 + Tables I.7 / I.8 Bits Per Component box (`bpcc`).
//!   * §I.5.3.3 + Figure I.10 / Tables I.9 / I.10 / I.11 Colour
//!     Specification box (`colr`) — both `METH = 1` enumerated
//!     (sRGB / greyscale / sYCC) and `METH = 2` ICC-profile paths.
//! * §I.5.4 Contiguous Codestream box (`jp2c`) length / offset
//!   arithmetic.
//!
//! Each byte along that path is attacker-controlled when a third party
//! hands us a `.jp2` sample, and any one of the box-length fields can
//! be malformed (zero-length, length shorter than the 8-byte header,
//! length overflowing the file). There is no external library oracle
//! worth pulling in, so this harness is parse-only: feed arbitrary
//! bytes and assert the call returns a `Result` rather than panicking,
//! integer-overflowing (debug), or indexing out of bounds.
//!
//! ## Input cap
//!
//! `parse_jp2` does not allocate per-pixel buffers, but the `bpcc` box
//! contents grow with `Csiz` and the `colr` ICC-profile contents can be
//! megabyte-scale in pathological inputs. Cap raw input at 256 KiB,
//! well above libFuzzer's default `-max_len` of 4 KiB but below any
//! realistic OOM threshold.

use libfuzzer_sys::fuzz_target;
use oxideav_jpeg2000::jp2;

const MAX_INPUT_BYTES: usize = 256 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }
    let _ = jp2::parse_jp2(data);
});
