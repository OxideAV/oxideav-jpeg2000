//! MQ arithmetic encoder (ISO/IEC 15444-1 Annex C).
//!
//! Mirror of the decoder in [`crate::decode::mqc`]. Uses the same 47
//! probability states and the complementary `INITENC` / `CODEMPS` /
//! `CODELPS` / `RENORME` / `BYTEOUT` / `FLUSH` primitives defined in
//! T.800 §C.2. Ported from OpenJPEG `mqc.c` (BSD-2-Clause).

use super::super::decode::mqc::{CTX_AGG, CTX_UNI, CTX_ZC, NUM_CTX};

/// One entry in the 47-state probability estimation table. Duplicated
/// here (the decoder has a private copy) so the encoder stays
/// self-contained and doesn't need a public symbol leaking out of the
/// decoder module.
#[derive(Clone, Copy)]
struct State {
    qeval: u32,
    mps: u8,
    nlps: u16,
    nmps: u16,
}

// Identical to the decoder's `STATES` — see T.800 Table C.1.
#[rustfmt::skip]
const STATES: [State; 94] = [
    State { qeval: 0x5601, mps: 0, nlps:  2, nmps:  3 },
    State { qeval: 0x5601, mps: 1, nlps:  3, nmps:  2 },
    State { qeval: 0x3401, mps: 0, nlps:  4, nmps: 12 },
    State { qeval: 0x3401, mps: 1, nlps:  5, nmps: 13 },
    State { qeval: 0x1801, mps: 0, nlps:  6, nmps: 18 },
    State { qeval: 0x1801, mps: 1, nlps:  7, nmps: 19 },
    State { qeval: 0x0ac1, mps: 0, nlps:  8, nmps: 24 },
    State { qeval: 0x0ac1, mps: 1, nlps:  9, nmps: 25 },
    State { qeval: 0x0521, mps: 0, nlps: 10, nmps: 58 },
    State { qeval: 0x0521, mps: 1, nlps: 11, nmps: 59 },
    State { qeval: 0x0221, mps: 0, nlps: 76, nmps: 66 },
    State { qeval: 0x0221, mps: 1, nlps: 77, nmps: 67 },
    State { qeval: 0x5601, mps: 0, nlps: 14, nmps: 13 },
    State { qeval: 0x5601, mps: 1, nlps: 15, nmps: 12 },
    State { qeval: 0x5401, mps: 0, nlps: 16, nmps: 28 },
    State { qeval: 0x5401, mps: 1, nlps: 17, nmps: 29 },
    State { qeval: 0x4801, mps: 0, nlps: 18, nmps: 28 },
    State { qeval: 0x4801, mps: 1, nlps: 19, nmps: 29 },
    State { qeval: 0x3801, mps: 0, nlps: 20, nmps: 28 },
    State { qeval: 0x3801, mps: 1, nlps: 21, nmps: 29 },
    State { qeval: 0x3001, mps: 0, nlps: 22, nmps: 34 },
    State { qeval: 0x3001, mps: 1, nlps: 23, nmps: 35 },
    State { qeval: 0x2401, mps: 0, nlps: 24, nmps: 36 },
    State { qeval: 0x2401, mps: 1, nlps: 25, nmps: 37 },
    State { qeval: 0x1c01, mps: 0, nlps: 26, nmps: 40 },
    State { qeval: 0x1c01, mps: 1, nlps: 27, nmps: 41 },
    State { qeval: 0x1601, mps: 0, nlps: 58, nmps: 42 },
    State { qeval: 0x1601, mps: 1, nlps: 59, nmps: 43 },
    State { qeval: 0x5601, mps: 0, nlps: 30, nmps: 29 },
    State { qeval: 0x5601, mps: 1, nlps: 31, nmps: 28 },
    State { qeval: 0x5401, mps: 0, nlps: 32, nmps: 28 },
    State { qeval: 0x5401, mps: 1, nlps: 33, nmps: 29 },
    State { qeval: 0x5101, mps: 0, nlps: 34, nmps: 30 },
    State { qeval: 0x5101, mps: 1, nlps: 35, nmps: 31 },
    State { qeval: 0x4801, mps: 0, nlps: 36, nmps: 32 },
    State { qeval: 0x4801, mps: 1, nlps: 37, nmps: 33 },
    State { qeval: 0x3801, mps: 0, nlps: 38, nmps: 34 },
    State { qeval: 0x3801, mps: 1, nlps: 39, nmps: 35 },
    State { qeval: 0x3401, mps: 0, nlps: 40, nmps: 36 },
    State { qeval: 0x3401, mps: 1, nlps: 41, nmps: 37 },
    State { qeval: 0x3001, mps: 0, nlps: 42, nmps: 38 },
    State { qeval: 0x3001, mps: 1, nlps: 43, nmps: 39 },
    State { qeval: 0x2801, mps: 0, nlps: 44, nmps: 38 },
    State { qeval: 0x2801, mps: 1, nlps: 45, nmps: 39 },
    State { qeval: 0x2401, mps: 0, nlps: 46, nmps: 40 },
    State { qeval: 0x2401, mps: 1, nlps: 47, nmps: 41 },
    State { qeval: 0x2201, mps: 0, nlps: 48, nmps: 42 },
    State { qeval: 0x2201, mps: 1, nlps: 49, nmps: 43 },
    State { qeval: 0x1c01, mps: 0, nlps: 50, nmps: 44 },
    State { qeval: 0x1c01, mps: 1, nlps: 51, nmps: 45 },
    State { qeval: 0x1801, mps: 0, nlps: 52, nmps: 46 },
    State { qeval: 0x1801, mps: 1, nlps: 53, nmps: 47 },
    State { qeval: 0x1601, mps: 0, nlps: 54, nmps: 48 },
    State { qeval: 0x1601, mps: 1, nlps: 55, nmps: 49 },
    State { qeval: 0x1401, mps: 0, nlps: 56, nmps: 50 },
    State { qeval: 0x1401, mps: 1, nlps: 57, nmps: 51 },
    State { qeval: 0x1201, mps: 0, nlps: 58, nmps: 52 },
    State { qeval: 0x1201, mps: 1, nlps: 59, nmps: 53 },
    State { qeval: 0x1101, mps: 0, nlps: 60, nmps: 54 },
    State { qeval: 0x1101, mps: 1, nlps: 61, nmps: 55 },
    State { qeval: 0x0ac1, mps: 0, nlps: 62, nmps: 56 },
    State { qeval: 0x0ac1, mps: 1, nlps: 63, nmps: 57 },
    State { qeval: 0x09c1, mps: 0, nlps: 64, nmps: 58 },
    State { qeval: 0x09c1, mps: 1, nlps: 65, nmps: 59 },
    State { qeval: 0x08a1, mps: 0, nlps: 66, nmps: 60 },
    State { qeval: 0x08a1, mps: 1, nlps: 67, nmps: 61 },
    State { qeval: 0x0521, mps: 0, nlps: 68, nmps: 62 },
    State { qeval: 0x0521, mps: 1, nlps: 69, nmps: 63 },
    State { qeval: 0x0441, mps: 0, nlps: 70, nmps: 64 },
    State { qeval: 0x0441, mps: 1, nlps: 71, nmps: 65 },
    State { qeval: 0x02a1, mps: 0, nlps: 72, nmps: 66 },
    State { qeval: 0x02a1, mps: 1, nlps: 73, nmps: 67 },
    State { qeval: 0x0221, mps: 0, nlps: 74, nmps: 68 },
    State { qeval: 0x0221, mps: 1, nlps: 75, nmps: 69 },
    State { qeval: 0x0141, mps: 0, nlps: 76, nmps: 70 },
    State { qeval: 0x0141, mps: 1, nlps: 77, nmps: 71 },
    State { qeval: 0x0111, mps: 0, nlps: 78, nmps: 72 },
    State { qeval: 0x0111, mps: 1, nlps: 79, nmps: 73 },
    State { qeval: 0x0085, mps: 0, nlps: 80, nmps: 74 },
    State { qeval: 0x0085, mps: 1, nlps: 81, nmps: 75 },
    State { qeval: 0x0049, mps: 0, nlps: 82, nmps: 76 },
    State { qeval: 0x0049, mps: 1, nlps: 83, nmps: 77 },
    State { qeval: 0x0025, mps: 0, nlps: 84, nmps: 78 },
    State { qeval: 0x0025, mps: 1, nlps: 85, nmps: 79 },
    State { qeval: 0x0015, mps: 0, nlps: 86, nmps: 80 },
    State { qeval: 0x0015, mps: 1, nlps: 87, nmps: 81 },
    State { qeval: 0x0009, mps: 0, nlps: 88, nmps: 82 },
    State { qeval: 0x0009, mps: 1, nlps: 89, nmps: 83 },
    State { qeval: 0x0005, mps: 0, nlps: 90, nmps: 84 },
    State { qeval: 0x0005, mps: 1, nlps: 91, nmps: 85 },
    State { qeval: 0x0001, mps: 0, nlps: 90, nmps: 86 },
    State { qeval: 0x0001, mps: 1, nlps: 91, nmps: 87 },
    State { qeval: 0x5601, mps: 0, nlps: 92, nmps: 92 },
    State { qeval: 0x5601, mps: 1, nlps: 93, nmps: 93 },
];

/// MQ arithmetic encoder state.
pub struct MqcEnc {
    /// Output bytes. `byteout` appends here; the leading byte is a
    /// sentinel 0 (never emitted) that OpenJPEG represents as the
    /// pre-start pointer `bp - 1`.
    out: Vec<u8>,
    /// Range register `A`.
    a: u32,
    /// Code register `C`.
    c: u32,
    /// Remaining bits to shift before the next byte emission.
    ct: u32,
    /// Current context state index for each of the 19 tier-1 contexts.
    ctxs: [u16; NUM_CTX],
    /// Currently selected context.
    curctx: usize,
}

impl Default for MqcEnc {
    fn default() -> Self {
        Self::new()
    }
}

impl MqcEnc {
    /// Create a fresh encoder.
    pub fn new() -> Self {
        let mut m = MqcEnc {
            // Sentinel byte for the "bp - 1" trick — `byteout` inspects
            // the last written byte to decide whether to bit-stuff.
            // A leading 0 guarantees the first comparison succeeds.
            out: vec![0u8],
            a: 0x8000,
            c: 0,
            ct: 12,
            ctxs: [0; NUM_CTX],
            curctx: 0,
        };
        m.reset_states();
        m
    }

    /// Reset all contexts to their tier-1 starting states, matching the
    /// EBCOT requirement at the start of each code-block.
    pub fn reset_states(&mut self) {
        for c in &mut self.ctxs {
            *c = 0;
        }
        // Tier-1 overrides identical to the decoder.
        self.set_state(CTX_UNI, 0, 46);
        self.set_state(CTX_AGG, 0, 3);
        self.set_state(CTX_ZC, 0, 4);
    }

    /// Point a specific context to a given (mps, probability row).
    pub fn set_state(&mut self, ctxno: usize, msb: u32, prob: u32) {
        self.ctxs[ctxno] = (msb + (prob << 1)) as u16;
    }

    /// Select a context for the following `encode` calls.
    #[inline]
    pub fn setcurctx(&mut self, ctxno: usize) {
        self.curctx = ctxno;
    }

    /// Encode one binary symbol (0 or 1) using the current context.
    #[inline]
    pub fn encode(&mut self, d: u32) {
        let state_ix = self.ctxs[self.curctx] as usize;
        let mps = STATES[state_ix].mps as u32;
        if mps == d {
            self.codemps();
        } else {
            self.codelps();
        }
    }

    #[inline]
    fn codemps(&mut self) {
        let state_ix = self.ctxs[self.curctx] as usize;
        let qeval = STATES[state_ix].qeval;
        self.a -= qeval;
        if (self.a & 0x8000) == 0 {
            if self.a < qeval {
                self.a = qeval;
            } else {
                self.c += qeval;
            }
            self.ctxs[self.curctx] = STATES[state_ix].nmps;
            self.renorme();
        } else {
            self.c += qeval;
        }
    }

    #[inline]
    fn codelps(&mut self) {
        let state_ix = self.ctxs[self.curctx] as usize;
        let qeval = STATES[state_ix].qeval;
        self.a -= qeval;
        if self.a < qeval {
            self.c += qeval;
        } else {
            self.a = qeval;
        }
        self.ctxs[self.curctx] = STATES[state_ix].nlps;
        self.renorme();
    }

    #[inline]
    fn renorme(&mut self) {
        loop {
            self.a <<= 1;
            self.c <<= 1;
            self.ct -= 1;
            if self.ct == 0 {
                self.byteout();
            }
            if (self.a & 0x8000) != 0 {
                break;
            }
        }
    }

    /// Flush the register state to the output stream — T.800 §C.2.9.
    pub fn flush(&mut self) {
        // SETBITS: C |= 0xFFFF, rolling back if the result would exceed
        // C+A.
        let tempc = self.c + self.a;
        self.c |= 0xFFFF;
        if self.c >= tempc {
            self.c -= 0x8000;
        }
        self.c <<= self.ct;
        self.byteout();
        self.c <<= self.ct;
        self.byteout();
        // It is forbidden that a coding pass end with 0xff — OpenJPEG
        // skips the trailing byte if it's not 0xff, otherwise keeps it
        // as the last byte. The end effect: `bp` always points past the
        // last *useful* byte.
        if *self.out.last().unwrap_or(&0) != 0xFF {
            // OK: the "bp++" in OpenJPEG corresponds to treating the
            // byte we already wrote as part of the output. Nothing to
            // do here — our `out` already includes it.
        }
    }

    /// Output a byte following OpenJPEG's `opj_mqc_byteout` rules with
    /// bit-stuffing after any emitted 0xFF. The first call happens with
    /// the leading sentinel byte already in `out`, so `last()` is
    /// guaranteed to return `Some`.
    fn byteout(&mut self) {
        let last = self.out.last().copied().unwrap_or(0);
        if last == 0xFF {
            self.out.push((self.c >> 20) as u8);
            self.c &= 0xFFFFF;
            self.ct = 7;
        } else if (self.c & 0x0800_0000) == 0 {
            self.out.push((self.c >> 19) as u8);
            self.c &= 0x7FFFF;
            self.ct = 8;
        } else {
            // Carry into the previously-written byte.
            let new_last = last.wrapping_add(1);
            if let Some(slot) = self.out.last_mut() {
                *slot = new_last;
            }
            if new_last == 0xFF {
                self.c &= 0x07FF_FFFF;
                self.out.push((self.c >> 20) as u8);
                self.c &= 0xFFFFF;
                self.ct = 7;
            } else {
                self.out.push((self.c >> 19) as u8);
                self.c &= 0x7FFFF;
                self.ct = 8;
            }
        }
    }

    /// Consume the encoder, returning the finalised byte stream.
    ///
    /// The leading sentinel byte is stripped. The trailing byte is
    /// preserved only if it is non-0xFF (matching OpenJPEG's
    /// `opj_mqc_flush` semantics). Call only once, after `flush`.
    pub fn finish(mut self) -> Vec<u8> {
        // Drop the leading sentinel.
        self.out.remove(0);
        // Trailing-0xFF rule: if the final byte is 0xFF, OpenJPEG's
        // `bp` stayed one short, which means that byte is *not* part of
        // the emitted stream. We mirror that by dropping it.
        if self.out.last() == Some(&0xFF) {
            self.out.pop();
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::mqc::{Mqc, CTX_ZC};

    /// Round-trip a short bit sequence through encode → decode.
    #[test]
    fn encode_decode_roundtrip_zc() {
        let bits = [0, 1, 1, 0, 0, 1, 0, 1, 1, 1, 0, 0, 1, 0, 1, 1, 0, 1, 0, 1];
        let mut enc = MqcEnc::new();
        enc.setcurctx(CTX_ZC);
        for &b in &bits {
            enc.encode(b);
        }
        enc.flush();
        let bytes = enc.finish();
        // Decode using the existing decoder.
        let mut dec = Mqc::init_dec(bytes);
        dec.resetstates();
        dec.setcurctx(CTX_ZC);
        let decoded: Vec<u32> = (0..bits.len()).map(|_| dec.decode()).collect();
        assert_eq!(decoded, bits.to_vec());
    }

    /// A longer all-MPS pattern should still decode back correctly.
    #[test]
    fn encode_decode_all_zeros() {
        let mut enc = MqcEnc::new();
        enc.setcurctx(CTX_ZC);
        for _ in 0..64 {
            enc.encode(0);
        }
        enc.flush();
        let bytes = enc.finish();
        let mut dec = Mqc::init_dec(bytes);
        dec.resetstates();
        dec.setcurctx(CTX_ZC);
        for _ in 0..64 {
            assert_eq!(dec.decode(), 0);
        }
    }
}
