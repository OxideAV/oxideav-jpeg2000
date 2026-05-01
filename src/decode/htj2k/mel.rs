//! MEL adaptive run-length decoder (§7.3.3 of ISO/IEC 15444-15:2019,
//! pages 12-13 of the FDIS).
//!
//! The MEL decoder consumes bits from the [`super::streams::MelReader`]
//! and produces 0/1 symbols `s^mel_q` used by the cleanup pass to flag
//! quads that are entirely inside an "all zero context" (AZC) region.
//! The decoder maintains an adaptive state index `MEL_k` that climbs
//! when long runs are observed and shrinks otherwise; the table
//! `MEL_E[k]` (Table 2 of §7.3.3) maps the state to the number of
//! exponent bits consumed when a run-length-zero terminator is found.
//!
//! All entries of the `MEL_E` table are reproduced verbatim from
//! Table 2 (page 13 of the FDIS).

use super::streams::MelReader;
use oxideav_core::Result;

/// Table 2 of §7.3.3 (page 13): MEL adaptive-state exponents.
/// Indexed by `MEL_k`, range `0..=12`.
pub const MEL_E: [u8; 13] = [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 4, 5];

/// MEL adaptive-state machine.
pub struct MelDecoder {
    k: u8,
    run: u32,
    one: u8,
}

impl Default for MelDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl MelDecoder {
    /// Spec procedure `initMELDecoder`.
    pub fn new() -> Self {
        Self {
            k: 0,
            run: 0,
            one: 0,
        }
    }

    /// Spec procedure `decodeMELSym`. Pulls bits from `reader` until
    /// the next MEL symbol is produced.
    pub fn decode_sym(&mut self, reader: &mut MelReader<'_>) -> Result<u8> {
        if self.run == 0 && self.one == 0 {
            let eval = MEL_E[self.k as usize];
            let bit = reader.import_bit()?;
            if bit == 1 {
                self.run = 1u32 << eval;
                self.k = (self.k + 1).min(12);
            } else {
                self.run = 0;
                let mut e = eval;
                while e > 0 {
                    let b = reader.import_bit()?;
                    self.run = 2 * self.run + b as u32;
                    e -= 1;
                }
                self.k = self.k.saturating_sub(1);
                self.one = 1;
            }
        }
        if self.run > 0 {
            self.run -= 1;
            Ok(0)
        } else {
            self.one = 0;
            Ok(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::htj2k::streams::compute_scup;

    fn mel_buf(bytes: &[u8]) -> Vec<u8> {
        // Build a cleanup segment whose first byte after Pcup is
        // `bytes[0]`. We want Pcup = 0 so the MEL reader starts at
        // index 0. Scup encodes via the trailing two bytes:
        //   Scup = 16 * Dcup[Lcup-1] + (Dcup[Lcup-2] & 0x0F)
        // For Lcup = N+2, choose Scup = Lcup → Dcup[Lcup-1] = 0,
        // Dcup[Lcup-2] = (Lcup & 0x0F) | 0x80 (top nibble 8 keeps the
        // MEL/VLC tail byte distinct from a stuffing-prone value).
        let mut v: Vec<u8> = bytes.to_vec();
        let lcup_after_tail = bytes.len() + 2;
        let low_nibble = (lcup_after_tail & 0x0F) as u8;
        v.push(0x80 | low_nibble);
        v.push(0x00);
        v
    }

    #[test]
    fn first_zero_bit_emits_a_one() {
        // MEL_k starts at 0, MEL_E[0] = 0. import bit 0 -> eval=0, bit=0,
        // run=0, k stays 0 (max(0,-1)), one=1. Then since run==0 and
        // one==1, return 1 and clear one.
        // First MEL byte must have MSB=0.  Use 0x00 → all zero bits.
        let buf = mel_buf(&[0x00]);
        let (pcup, _) = compute_scup(&buf).unwrap();
        let mut r = MelReader::new(&buf, pcup);
        let mut d = MelDecoder::new();
        let s = d.decode_sym(&mut r).unwrap();
        assert_eq!(s, 1);
    }

    #[test]
    fn first_one_bit_starts_run() {
        // MEL_k starts at 0, MEL_E[0] = 0. import bit 1 -> run = 1<<0 = 1,
        // k becomes 1. Then run > 0, decrement to 0, return 0.
        // 0x80 has MSB=1, then zeros.
        let buf = mel_buf(&[0x80]);
        let (pcup, _) = compute_scup(&buf).unwrap();
        let mut r = MelReader::new(&buf, pcup);
        let mut d = MelDecoder::new();
        let s = d.decode_sym(&mut r).unwrap();
        assert_eq!(s, 0);
        // Next call: run==0, one==0, eval=MEL_E[1]=0, next bit = 0
        // (the next bit of 0x80 is 0) → return 1.
        let s = d.decode_sym(&mut r).unwrap();
        assert_eq!(s, 1);
    }
}
