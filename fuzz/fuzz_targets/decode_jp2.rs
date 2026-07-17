#![no_main]

//! Panic-free fuzz target for the end-to-end **JP2 file decode** path
//! reached through [`oxideav_jpeg2000::jp2::decode_jp2`] — everything
//! the parse-only `parse_jp2` harness stops short of:
//!
//! * the T.800 Annex I box walk (all of `parse_jp2`), plus the round-416
//!   `pclr` / `cmap` / `cdef` / `res ` box parsers — palette column
//!   layouts (1–38-bit, signed, padded non-multiple-of-8 storage),
//!   component-mapping entries, channel definitions and grid
//!   resolutions, with their cross-box pairing / index-range rules;
//! * the contiguous-codestream decode (`decode_j2k` on the `jp2c`
//!   payload span);
//! * the §I.5.3.4 / §I.5.3.5 palette application (per-sample column
//!   lookups with index clamping) and the §I.5.3.6 `cdef` presentation
//!   reorder.
//!
//! Every byte is attacker-controlled when a third party hands us a
//! `.jp2` file. The harness is oracle-free: feed arbitrary bytes and
//! assert the call returns a `Result` rather than panicking, indexing
//! out of bounds, or overflowing (debug) — decoded channels are
//! discarded.
//!
//! ## Input + geometry caps
//!
//! Raw input is capped at 64 KiB. Like the `decode_j2k` harness, the
//! decoder allocates per-pixel buffers from the attacker-controlled
//! `SIZ` geometry, so the harness pre-parses the embedded codestream
//! header and skips files whose reference-grid area exceeds 2²⁰
//! samples or that carry more than 4 components. The palette
//! expansion multiplies planes by `NPC`, so the `cmap` channel count
//! is capped at 8.

use libfuzzer_sys::fuzz_target;
use oxideav_jpeg2000::{jp2, parse_j2k_header};

const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_AREA: u64 = 1 << 20;
const MAX_COMPONENTS: usize = 4;
const MAX_CHANNELS: usize = 8;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }
    let Ok(container) = jp2::parse_jp2(data) else {
        return;
    };
    if container
        .header
        .cmap
        .as_ref()
        .is_some_and(|m| m.len() > MAX_CHANNELS)
    {
        return;
    }
    let Some(end) = container
        .codestream_offset
        .checked_add(container.codestream_len)
    else {
        return;
    };
    let Some(codestream) = data.get(container.codestream_offset..end) else {
        return;
    };
    let Ok(header) = parse_j2k_header(codestream) else {
        return;
    };
    let area = u64::from(header.siz.x_size) * u64::from(header.siz.y_size);
    if area > MAX_AREA || header.siz.components.len() > MAX_COMPONENTS {
        return;
    }
    let _ = jp2::decode_jp2(data);
});
