//! HT MagRef pass encoder (inverse of
//! [`crate::decode::htj2k::magref::decode_magref`]).
//!
//! The MagRef pass emits one refinement bit per already-significant sample
//! (σ_n = 1 after the cleanup pass). There is no neighbourhood test and
//! no sign step — every significant sample unconditionally contributes
//! one bit.
//!
//! The resulting bits are packed into a **reverse** LSB-first byte stream
//! (same stuffing rule as VLC: when the previously written byte is > 0x8F
//! and the new byte's low 7 bits are all 1, only 7 payload bits are
//! used and the MSB is forced to 0). This stream forms the **tail** of
//! the `Dref` HT refinement segment, read backward by the decoder's
//! `MagRefReader`.

use crate::decode::htj2k::CleanupOutput;
use crate::error::Result;

/// Encode the MagRef pass for a code-block. `ref_bit[n]` is the
/// next-magnitude refinement bit for sample n. Only samples with
/// `cleanup.sig[n] = 1` are encoded; the rest are ignored.
///
/// Returns the byte sequence to be concatenated **after** the SigProp
/// bytes in `Dref` (i.e. appended in reverse order so the decoder reads
/// them from `Dref[Lref-1]` backward).
pub fn encode_magref(cleanup: &CleanupOutput, ref_bit: &[u8]) -> Result<Vec<u8>> {
    // Collect the refinement bits for significant samples in forward
    // sample order (n = 0, 1, ...). The decoder reads them in the same
    // order via MagRefReader which walks `Dref` backward.
    let mut bits: Vec<u8> = Vec::new();
    for n in 0..cleanup.sig.len() {
        if cleanup.sig[n] != 0 {
            let b = ref_bit.get(n).copied().unwrap_or(0) & 1;
            bits.push(b);
        }
    }
    // Pack bits into a reverse stream: collect bytes LSB-first, then
    // apply the MagRef stuffing rule (last > 0x8F + next 7 LSB all 1 →
    // skip one bit slot), then reverse so the byte the decoder reads first
    // is the last in our emitted sequence.
    let bytes = pack_magref_bits(&bits);
    Ok(bytes)
}

/// Pack a sequence of bits into the MagRef reverse byte stream.
///
/// Encoding: bits are placed LSB-first into bytes. After any byte B where
/// `B > 0x8F` (i.e. the byte, considered as a 7-bit VLC payload, had its
/// full low-7-bits set to 1), the next byte has only 7 payload bits
/// (bit 7 forced 0). The resulting `bytes_fwd` are reversed before
/// returning so that the decoder's `MagRefReader` (which starts at
/// `Dref[Lref-1]` and walks backward) sees them in the correct order.
fn pack_magref_bits(bits: &[u8]) -> Vec<u8> {
    if bits.is_empty() {
        return Vec::new();
    }
    // `last` tracks the previously-emitted byte (in forward-emit order)
    // so we can apply the stuffing predicate correctly.
    let mut bytes_fwd: Vec<u8> = Vec::new();
    let mut cur: u8 = 0;
    let mut nbits: u8 = 0;
    let mut last: u8 = 0xFF; // initial value per spec (decoder's MagRefReader init)
    let mut have_last = false;

    let mut idx = 0;
    while idx < bits.len() {
        // Determine capacity of this byte.
        let cap: u8 = if have_last && last > 0x8F && (cur & 0x7F) == 0x7F {
            // Stuffing: only 7 payload bits; MSB forced to 0.
            // Note: this is the predicate on the PREVIOUS byte (last),
            // and the NEXT byte's low 7 bits. We have to peek.
            // Since we're building the next byte, cap = 7.
            7
        } else {
            8
        };

        // Fill up to `cap` bits into `cur` LSB-first.
        let available = bits.len() - idx;
        let take = (cap - nbits) as usize;
        let take = take.min(available);
        for _ in 0..take {
            let b = bits[idx] & 1;
            cur |= b << nbits;
            nbits += 1;
            idx += 1;
        }

        // When the byte is full (or we ran out of bits), flush.
        if nbits == cap || idx == bits.len() {
            bytes_fwd.push(cur);
            last = cur;
            have_last = true;
            cur = 0;
            nbits = 0;
        }
    }

    // Reverse so the decoder's backward reader sees them in forward-bit order.
    bytes_fwd.reverse();
    bytes_fwd
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::htj2k::{decode_codeblock, CleanupOutput, ZBlk};
    use crate::encode::htj2k::cleanup_enc::{encode_cleanup, SampleHt};

    /// All-zero block: no significant samples → MagRef emits nothing.
    #[test]
    fn magref_enc_all_zero_no_bits() {
        let n = 16usize;
        let cleanup = CleanupOutput {
            width: 4,
            height: 4,
            mag: vec![0u64; n],
            sign: vec![0u8; n],
            exp: vec![0u8; n],
            sig: vec![0u8; n],
        };
        let ref_bit = vec![0u8; n];
        let dref_tail = encode_magref(&cleanup, &ref_bit).expect("magref enc");
        assert!(dref_tail.is_empty());
    }

    /// One significant sample: MagRef must emit one bit. Verify via
    /// end-to-end ZBlk::Three decode with the assembled Dref.
    #[test]
    fn magref_enc_single_sig_sample_e2e() {
        let mut samples = vec![SampleHt::default(); 16];
        samples[0] = SampleHt { mag: 1, sign: 0 };
        let dcup = encode_cleanup(4, 4, &samples).expect("cleanup enc");

        // Build CleanupOutput: sig[0]=1, rest=0.
        let n = 16usize;
        let cleanup = CleanupOutput {
            width: 4,
            height: 4,
            mag: {
                let mut v = vec![0u64; n];
                v[0] = 1;
                v
            },
            sign: vec![0u8; n],
            exp: vec![0u8; n],
            sig: {
                let mut v = vec![0u8; n];
                v[0] = 1;
                v
            },
        };

        // Emit ref_bit[0] = 1 (the significant sample gets a 1-bit).
        let mut ref_bit = vec![0u8; n];
        ref_bit[0] = 1;
        let dref_tail = encode_magref(&cleanup, &ref_bit).expect("magref enc");
        // Should have emitted at least one byte.
        assert!(!dref_tail.is_empty(), "expected non-empty dref");

        // Decode via ZBlk::Three. Dref = dref_tail (no SigProp bytes).
        // The decoder reads MagRef from the tail of dref.
        let out = decode_codeblock(4, 4, ZBlk::Three, &dcup, &dref_tail).expect("dec Z3");
        // The significant sample should have z=1.
        assert_eq!(out.z[0], 1, "z[0] must be 1 for significant sample");
        // r[0] should match our emitted bit = 1.
        assert_eq!(out.refinement[0], 1, "r[0] must be 1");
    }

    /// Multiple significant samples: MagRef bits emitted in sample order,
    /// decoded correctly by ZBlk::Three.
    #[test]
    fn magref_enc_multiple_sig_samples_e2e() {
        // All-ones 2x2 block (all four samples significant, mag=1).
        let samples = vec![SampleHt { mag: 1, sign: 0 }; 4];
        let dcup = encode_cleanup(2, 2, &samples).expect("cleanup enc");

        let n = 4usize;
        let cleanup = CleanupOutput {
            width: 2,
            height: 2,
            mag: vec![1u64; n],
            sign: vec![0u8; n],
            exp: vec![0u8; n],
            sig: vec![1u8; n],
        };
        // Alternating 0/1 refinement bits.
        let ref_bit: Vec<u8> = (0..n).map(|i| (i & 1) as u8).collect();
        let dref_tail = encode_magref(&cleanup, &ref_bit).expect("magref enc");

        let out = decode_codeblock(2, 2, ZBlk::Three, &dcup, &dref_tail).expect("dec Z3");
        for i in 0..n {
            assert_eq!(out.z[i], 1, "z[{i}] must be 1");
            assert_eq!(out.refinement[i], ref_bit[i], "r[{i}] mismatch");
        }
    }
}
