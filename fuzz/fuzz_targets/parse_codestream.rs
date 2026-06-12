#![no_main]

//! Panic-free fuzz target for the top-level JPEG 2000 codestream parser
//! reached through [`oxideav_jpeg2000::parse_codestream`].
//!
//! Exercises the chain documented in `docs/image/jpeg2000/`:
//!
//! * T.800 §A.4 delimiting markers — SOC outer framing, SOT / SOD
//!   tile-part boundaries, EOC terminator, with both fixed-`Psot` and
//!   `Psot == 0` ("body until EOC") framings per §A.4.2.
//! * T.800 §A.5.1 SIZ main-header parsing (Tables A.9 / A.10 / A.11)
//!   including the per-component `Ssiz` / `XRsiz` / `YRsiz` triples
//!   whose count is itself attacker-controlled via `Csiz`.
//! * T.800 §A.6.1 COD parsing (Tables A.12..A.21) with its
//!   variable-length precinct-byte tail keyed on `NL`.
//! * T.800 §A.6.4 QCD parsing (Tables A.27..A.30) with the three
//!   quantisation styles (reversible byte-stream, irreversible
//!   word-stream, scalar-derived single-pair).
//! * T.800 §A.2 / Tables A.2 / A.3 marker allow-lists used to
//!   validate the tile-part walker — the parser rejects forbidden
//!   markers (`SOC`, `SIZ`, `CAP`, `PRF`, `CRG`, `TLM`, `PLM`, `PPM`)
//!   when they appear inside a tile-part header.
//! * T.800 §A.6.2 / A.6.3 / A.6.5 / A.6.6 / A.7.3 / A.7.5 / A.9.2 — the
//!   typed tile-part markers `COC`, `RGN`, `QCC`, `POC`, `PLT`, `PPT`,
//!   `COM`.
//!
//! Every byte along that path is attacker-controlled when a third party
//! hands us a `.j2k` sample. This harness is oracle-free and
//! parse-only: feed arbitrary bytes and assert the
//! call returns a `Result` rather than panicking, integer-overflowing
//! (debug), indexing out of bounds, or allocating an attacker-controlled
//! buffer.
//!
//! ## Input cap
//!
//! `parse_codestream` does not allocate per-pixel buffers (full decode
//! returns `Error::NotImplemented`), but its `Vec<TilePart>` /
//! `Vec<TilePartMarker>` / `Vec<SizComponent>` growth is bounded by the
//! caller's input length: a tile-part chain or `Csiz` field that's
//! larger than the input itself will fail the bounds check before the
//! `Vec` can grow. Cap raw input at 64 KiB anyway — well above
//! libFuzzer's default `-max_len` of 4 KiB — as defence in depth against
//! a runner override or a degenerate `Psot == 0` stream that walks the
//! entire input.

use libfuzzer_sys::fuzz_target;
use oxideav_jpeg2000::parse_codestream;

const MAX_INPUT_BYTES: usize = 64 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }
    let _ = parse_codestream(data);
});
