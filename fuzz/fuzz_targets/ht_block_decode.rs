#![no_main]

//! Structure-aware fuzz target driving the T.814 clause-7 **HT block
//! decoder** ([`oxideav_jpeg2000::ht::decode_ht_codeblock`]) directly
//! with attacker-controlled codeword segments.
//!
//! The whole-stream `decode_j2k` harness only reaches the HT block
//! decoder through a fully conformant tier-2 framing, so the deep
//! clause-7 state machines see mostly valid prefixes there. This
//! target hands the decoder raw bytes as its §B.2 cleanup and
//! refinement segments under a fuzz-chosen geometry, hitting the 7.1
//! bit-stream recovery machines (MagSgn / MEL / VLC and the forward /
//! backward SigProp / MagRef readers, each with its `0xFF`-stuffing
//! rule), the 7.3.3 MEL run-length decoder, the 7.3.5 CxtVLC tables,
//! the 7.3.6 U-VLC prefix / suffix / extension (first-line-pair
//! interleave included), the 7.3.8 MagSgn recovery, and the 7.4 / 7.5
//! refinement passes — plus the §7.6 conformance guards (`μ` width vs
//! `S_blk`, `Mb` positioning) on hostile values.
//!
//! The first bytes script the block shape:
//!
//! * sub-band orientation (context formation differs for HL / HH),
//! * width × height (each ≥ 1, area ≤ 4096 — the Table A.18 block cap),
//! * `Mb` (1..=37), `Z_blk` (0..=3), `S_blk` (0..=36),
//! * the cleanup / refinement split point in the remaining bytes.
//!
//! The harness is oracle-free: every call must return a `Result`
//! rather than panicking, indexing out of bounds, or overflowing
//! (debug).

use libfuzzer_sys::fuzz_target;
use oxideav_jpeg2000::geometry::SubBandOrientation;
use oxideav_jpeg2000::ht::decode_ht_codeblock;

const MAX_INPUT_BYTES: usize = 16 * 1024;
const MAX_BLOCK_AREA: usize = 4096;

fuzz_target!(|data: &[u8]| {
    if data.len() < 8 || data.len() > MAX_INPUT_BYTES {
        return;
    }
    let orientation = match data[0] % 4 {
        0 => SubBandOrientation::LL,
        1 => SubBandOrientation::HL,
        2 => SubBandOrientation::LH,
        _ => SubBandOrientation::HH,
    };
    let width = 1 + usize::from(data[1]) % 128;
    let height = 1 + usize::from(data[2]) % (MAX_BLOCK_AREA / width).min(128);
    let mb = 1 + u32::from(data[3]) % 37;
    let z_blk = data[4] % 4;
    let s_blk = u32::from(data[5]) % 37;

    let body = &data[8..];
    let split = usize::from(u16::from_be_bytes([data[6], data[7]])) % (body.len() + 1);
    let (cleanup, refinement) = body.split_at(split);

    let _ = decode_ht_codeblock(
        orientation,
        width,
        height,
        mb,
        cleanup,
        refinement,
        z_blk,
        s_blk,
    );
});
