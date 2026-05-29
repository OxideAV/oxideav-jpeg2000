#![no_main]

//! Panic-free fuzz target for the JPEG 2000 main-header parser reached
//! through [`oxideav_jpeg2000::parse_j2k_header`].
//!
//! This is the lower-level entry point that stops at the end of the
//! main header (after SIZ / COD / QCD, before the first SOT). It is the
//! parser used by tools that only want the SIZ-derived geometry (image
//! area, component count, sub-sampling) or COD-derived progression-order
//! information without walking the tile-part chain.
//!
//! Exercises the same T.800 §A.4 / §A.5.1 / §A.6.1 / §A.6.4 surface as
//! the [`parse_codestream`] harness but at a higher rate per second
//! (no tile-part walk, no per-tile-part marker parsing). Useful for
//! steering libFuzzer into the SIZ component-table arithmetic and the
//! COD variable-length precinct-byte tail keyed on `NL` (which can be
//! `0..=32` per Table A.15) without spending coverage budget on the
//! tile-part chain.
//!
//! ## Input cap
//!
//! The main header alone is small in well-formed streams (typically
//! under 64 bytes for SOC + SIZ + COD + QCD plus a handful of optional
//! markers), but the `Csiz` field declares up to `16384` components per
//! Table A.10 and the SIZ payload grows linearly with `Csiz`. Cap raw
//! input at 256 KiB so libFuzzer can find the maximum-`Csiz` corner
//! without OOM.

use libfuzzer_sys::fuzz_target;
use oxideav_jpeg2000::parse_j2k_header;

const MAX_INPUT_BYTES: usize = 256 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }
    let _ = parse_j2k_header(data);
});
