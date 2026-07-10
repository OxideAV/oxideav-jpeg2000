#![no_main]

//! Panic-free fuzz target for the **full decode** path reached through
//! [`oxideav_jpeg2000::decode_j2k`] — everything the parse-only
//! `parse_codestream` harness stops short of:
//!
//! * T.800 §B.10 packet-header reading (tag trees, coding-pass
//!   codewords, `Lblock` length fields) across all five §B.12.1
//!   progression walks, with `PPM` / `PPT` relocation and SOP / EPH
//!   framing when signalled.
//! * The §B.10.7 codeword-segment accumulation for every
//!   [`SegmentSplit`] shape — single-segment, §D.4.2 per-pass, §D.6
//!   bypass spans, and the T.814 HT set-`T` split including the §B.1
//!   placeholder-pass pinning and the §B.3 MULTIHT set grouping whose
//!   per-set allocations are bounded by the band's bit-plane budget.
//! * Tier-1 itself: the Annex C MQ decoder + Annex D passes on T.800
//!   blocks, and the T.814 clause-7 HT block decoder (MEL / VLC /
//!   MagSgn / SigProp / MagRef bit-stream recovery) on HT blocks.
//! * Dequantisation, inverse DWT and the §G component transforms into
//!   the final sample planes.
//!
//! Every byte is attacker-controlled when a third party hands us a
//! `.j2k` sample. The harness is oracle-free: feed arbitrary bytes and
//! assert the call returns a `Result` rather than panicking, indexing
//! out of bounds, or overflowing (debug) — decoded pixels are
//! discarded.
//!
//! ## Input + geometry caps
//!
//! Raw input is capped at 64 KiB. `decode_j2k` allocates per-pixel
//! buffers from the attacker-controlled `SIZ` geometry, so the harness
//! pre-parses the header and skips streams whose reference-grid area
//! exceeds 2²⁰ samples or that carry more than 4 components — large
//! enough to reach every code path (multi-tile, sub-sampling,
//! multi-component), small enough that a fuzz iteration never
//! allocates attacker-scaled memory.

use libfuzzer_sys::fuzz_target;
use oxideav_jpeg2000::{decode_j2k, parse_j2k_header};

const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_AREA: u64 = 1 << 20;
const MAX_COMPONENTS: usize = 4;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }
    let Ok(header) = parse_j2k_header(data) else {
        return;
    };
    let area = u64::from(header.siz.x_size) * u64::from(header.siz.y_size);
    if area > MAX_AREA || header.siz.components.len() > MAX_COMPONENTS {
        return;
    }
    let _ = decode_j2k(data);
});
