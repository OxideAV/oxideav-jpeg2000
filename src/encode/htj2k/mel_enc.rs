//! MEL adaptive run-length encoder (inverse of
//! [`crate::decode::htj2k::mel`]).
//!
//! T.814 §7.3.3 specifies the decoder's adaptive state machine; the
//! encoder mirrors it exactly. We accept a stream of `0` / `1` symbols
//! and produce the bit-sequence the decoder must consume to recover
//! that stream.
//!
//! Per the decoder's `decode_sym` procedure (page 13 of the FDIS):
//!
//! ```text
//!   if run == 0 && one == 0:
//!     eval = MEL_E[k]
//!     bit = importBit
//!     if bit == 1: run = 1 << eval, k = min(k+1, 12)
//!     else:        run = 0; for i in eval: bit; run = 2*run + bit; k--; one = 1
//!   if run > 0: run--; emit 0
//!   else:       one = 0; emit 1
//! ```
//!
//! On encode, we walk an internal `MelDecoder`-like state and decide
//! per symbol whether we are still inside an in-flight run or whether
//! we need to "start" a new chunk. Starting a new chunk means we get
//! to choose between:
//!
//! * Emitting a `1` bit followed by a `2^eval`-symbol run of zeros
//!   (the "long-run" branch). To take this branch we need the next
//!   `2^eval` symbols to all be `0`.
//! * Emitting a `0` bit, then `eval` more bits encoding a length
//!   `r ∈ [0, 2^eval - 1]` of zero symbols, and finally a `1` symbol
//!   (the "short-run" branch). The decoder consumes `r` zeros and
//!   then one `1`.
//!
//! When the encoder reaches end-of-stream while inside a partial
//! "long-run" we have two choices: pad zero symbols until the run
//! completes (matches the decoder's behaviour because trailing samples
//! in the cleanup pass are AZC quads anyway), or close the run. The
//! cleanup encoder here always provides the exact symbol count up
//! front, so we compute the optimal sequence of branches.

use super::streams_enc::MelWriter;

const MEL_E: [u8; 13] = [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 4, 5];

/// Encode a symbol sequence. Each `0` corresponds to "this quad is
/// AZC" (skip cleanup-pass output for one quad); each `1` corresponds
/// to "this quad is not AZC" (the corresponding CxtVLC entry follows
/// in the VLC stream).
pub fn encode_mel_symbols(syms: &[u8]) -> Vec<u8> {
    let mut writer = MelWriter::new();
    let mut k: u8 = 0;
    let mut i = 0usize;
    while i < syms.len() {
        let eval = MEL_E[k as usize];
        let max_run = 1u32 << eval;
        // Count leading zeros from position `i`, capped at `max_run`.
        let mut zeros = 0u32;
        while (i + zeros as usize) < syms.len() && zeros < max_run && syms[i + zeros as usize] == 0
        {
            zeros += 1;
        }
        let next_is_one = (i + zeros as usize) < syms.len() && syms[i + zeros as usize] == 1;

        if zeros == max_run {
            // Long-run branch: bit=1 consumes 2^eval zero symbols.
            writer.write_bit(1);
            i += max_run as usize;
            k = (k + 1).min(12);
        } else if next_is_one {
            // Short-run branch: bit=0 then `eval` bits of run-length,
            // then the implicit terminator `1` symbol consumes one
            // sample's worth of "1" output.
            writer.write_bit(0);
            // Emit the `eval`-bit run length MSB-first per the decoder's
            // `run = 2 * run + bit` accumulation.
            for s in (0..eval).rev() {
                let b = ((zeros >> s) & 1) as u8;
                writer.write_bit(b);
            }
            i += zeros as usize + 1; // consumed `zeros` 0s + one 1
            k = k.saturating_sub(1);
        } else {
            // We hit end-of-stream inside a partial run of zeros (no
            // terminating 1 within max_run). The decoder, given a
            // long-run prefix bit, will subtract zeros until the
            // segment is exhausted; trailing samples then default to
            // "no MEL symbol consumed" because the cleanup pass walks
            // exactly `nquads` quads. Pad with the long-run branch so
            // any leftover quads are AZC zeros which the caller has
            // already promised by feeding 0s.
            writer.write_bit(1);
            // The decoder will emit `max_run` zeros; we only had
            // `zeros < max_run` real zeros. Since trailing AZC quads
            // are valid (the cleanup decoder reads no extra bits for
            // them), this is safe — the encoder simply emits a longer
            // run than strictly needed and the cleanup loop terminates
            // first.
            i += max_run as usize;
            k = (k + 1).min(12);
        }
    }
    writer.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::htj2k::mel::MelDecoder;
    use crate::decode::htj2k::streams::{compute_scup, MelReader};

    fn roundtrip(syms: &[u8]) {
        let mel_bytes = encode_mel_symbols(syms);
        // Build a synthetic Dcup: MEL bytes start at Pcup. Choose
        // Pcup = 0 by setting Scup = mel_bytes.len() + 2.
        let mut dcup = mel_bytes.clone();
        let scup = mel_bytes.len() + 2;
        dcup.push((scup & 0x0F) as u8 | 0x80); // low nibble of Scup, top
                                               // nibble = 8 to stay clean
        dcup.push(((scup >> 4) & 0xFF) as u8);
        // Adjust if Scup overflows nibble; for our small test cases
        // scup < 16 so we're safe.
        assert!(scup < 16);
        let (pcup, _) = compute_scup(&dcup).unwrap();
        assert_eq!(pcup, 0);
        let mut r = MelReader::new(&dcup, pcup);
        let mut d = MelDecoder::new();
        let mut got = Vec::new();
        for _ in 0..syms.len() {
            got.push(d.decode_sym(&mut r).unwrap());
        }
        assert_eq!(got, syms, "MEL round-trip failed");
    }

    #[test]
    fn mel_roundtrip_single_zero() {
        roundtrip(&[0]);
    }

    #[test]
    fn mel_roundtrip_single_one() {
        roundtrip(&[1]);
    }

    #[test]
    fn mel_roundtrip_zero_then_one() {
        roundtrip(&[0, 1]);
    }

    #[test]
    fn mel_roundtrip_many_zeros() {
        roundtrip(&[0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn mel_roundtrip_alternating() {
        roundtrip(&[0, 1, 0, 1, 0, 1, 0, 1]);
    }

    #[test]
    fn mel_roundtrip_all_ones() {
        roundtrip(&[1, 1, 1, 1]);
    }

    #[test]
    fn mel_roundtrip_run_then_one() {
        roundtrip(&[0, 0, 0, 0, 1]);
    }

    #[test]
    fn mel_roundtrip_long_zero_run() {
        let mut syms = vec![0u8; 32];
        syms.push(1);
        roundtrip(&syms);
    }
}
