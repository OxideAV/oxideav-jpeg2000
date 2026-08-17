#![no_main]

//! Panic-free fuzz target for the two **partial-decode** surfaces the
//! full-decode harness never reaches:
//!
//! * [`oxideav_jpeg2000::decode_j2k_reduced`] — the ISO/IEC 15444-4
//!   §B.2.3 reduced-resolution decode: the §F.3.1 synthesis stops
//!   `discard_levels` short, every band / precinct / code-block corner
//!   re-maps through the Equation B-14 ceiling division, and the
//!   discarded levels' code-blocks skip tier-1 entirely — a distinct
//!   geometry walk with its own truncation edge cases.
//! * [`oxideav_jpeg2000::decode_j2k_layers`] — the layer-progressive
//!   decode: each code-block keeps only the coding passes its first
//!   `max_layers` quality layers carried, exercising the truncated
//!   per-coefficient `Nb(u, v)` reconstruction and the §B.10.7
//!   segment-length bookkeeping at every prefix.
//!
//! Both walk the same attacker-controlled tier-2 / tier-1 surface as
//! `decode_j2k` but through different cursors and budgets, so a stream
//! that full-decodes cleanly can still mis-index a reduced or
//! layer-limited walk. The harness is oracle-free: feed arbitrary
//! bytes, call each variant at two depths, and assert every call
//! returns a `Result` rather than panicking, indexing out of bounds,
//! or overflowing (debug).
//!
//! ## Input + geometry caps
//!
//! Identical to the `decode_j2k` harness (64 KiB, 2²⁰ reference-grid
//! samples, ≤ 4 components) so the two targets share seed corpora.

use libfuzzer_sys::fuzz_target;
use oxideav_jpeg2000::{decode_j2k_layers, decode_j2k_reduced, parse_j2k_header};

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
    let _ = decode_j2k_reduced(data, 1);
    let _ = decode_j2k_reduced(data, 3);
    let _ = decode_j2k_layers(data, 1);
    let _ = decode_j2k_layers(data, 2);
});
