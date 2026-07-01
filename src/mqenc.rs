//! Tier-1 MQ arithmetic **encoder** — T.800 Annex C (encoder side, §C.2).
//!
//! This is the compressing counterpart of the [`crate::mq`] decoder: the
//! binary adaptive arithmetic coder the tier-1 EBCOT bit-plane passes
//! (Annex D) drive when *producing* a codestream. Each call to
//! [`MqEncoder::encode`] takes a context label (an [`crate::mq::MqContext`])
//! and a binary decision `D ∈ {0, 1}` and folds it into the interval; when
//! all decisions have been fed, [`MqEncoder::flush`] terminates the segment
//! and hands back the compressed byte string.
//!
//! ## What this module covers
//!
//! The normative encoder procedures of T.800 §C.2 (Figures C.2–C.12),
//! sharing the Table C.2 [`crate::mq::QE`] probability-estimation rows and
//! the Table D.7 initial states with the decoder:
//!
//! * [`MqEncoder::new`] — INITENC (§C.2.8, Figure C.10). `A = 0x8000`,
//!   `C = 0`, `CT = 12`, `BP` parked before the first output byte.
//! * [`MqEncoder::encode`] — ENCODE (§C.2.2) dispatching to CODE1 /
//!   CODE0 → CODEMPS (§C.2.4, Figure C.7) / CODELPS (§C.2.4, Figure C.6),
//!   with the conditional MPS/LPS exchange and the §C.2.5 probability
//!   update embedded.
//! * `renorme` — RENORME (§C.2.6, Figure C.8). Shifts `A`/`C` left,
//!   emitting a byte via BYTEOUT whenever `CT` counts down to zero.
//! * `byteout` — BYTEOUT (§C.2.7, Figure C.9), including the bit-stuffing
//!   after a `0xFF` byte and the carry-propagation handling that make it
//!   impossible for a carry to reach past the most-recently-written byte.
//! * [`MqEncoder::flush`] — FLUSH (§C.2.9, Figures C.11 / C.12): SETBITS
//!   forces the low bits of `C` to `1` up to the interval bound, two
//!   BYTEOUT shifts push the tail out, and a trailing `0xFF` byte is
//!   discarded so the terminating marker overlaps the last data bits.
//!
//! ## Register conventions (T.800 §C.2.1, Table C.1)
//!
//! The 32-bit `C` register is laid out `0000 cbbb bbbb bsss xxxx xxxx
//! xxxx xxxx`: bit 27 is the carry `c`, bits 26..=19 are the completed
//! byte `b`, bits 18..=16 are the spacer `sss`, and bits 15..=0 are the
//! fractional `x`. `A` is the 16-bit interval; `CT` counts renormalization
//! shifts down to a BYTEOUT boundary.
//!
//! ## Round-trip contract
//!
//! The encoder is the exact inverse of [`crate::mq::MqDecoder`]: feeding a
//! decision stream through [`MqEncoder::encode`] + [`MqEncoder::flush`]
//! and then reading the resulting bytes back through the decoder with the
//! same per-context state reproduces the original decisions bit-for-bit.
//! The unit tests assert this over long pseudo-random streams and over the
//! adversarial all-`0xFF` and carry-heavy cases.
//!
//! ## Clean-room provenance
//!
//! Implemented solely from `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf`
//! (Annex C §C.2 prose + Table C.1 + Table C.2; Annex D §D.4 Table D.7).
//! The §C.2 figures are flowcharts; their prose descriptions are
//! transcribed to integer operations and validated by round-trip against
//! the independently-written §C.3 decoder.

use crate::mq::{MqContext, QE};

/// The MQ arithmetic encoder of T.800 §C.2.
///
/// Holds the §C.2.1 register state (`A`, `C`, `CT`) and the growing
/// compressed-byte buffer. Per-context adaptive state lives in the
/// caller's [`MqContext`] values, exactly mirroring the decoder's
/// stateless-with-respect-to-contexts model.
#[derive(Debug, Clone)]
pub struct MqEncoder {
    /// Interval register `A` (16-bit value held in a `u32`).
    a: u32,
    /// Code register `C` (the §C.2.1 layout; held in a `u64` so the
    /// FLUSH `C <<= CT` shifts never lose the high bits BYTEOUT extracts).
    c: u64,
    /// Bit counter `CT` (§C.2.6).
    ct: i32,
    /// Output buffer of completed compressed bytes.
    ///
    /// `BP` in the spec is modelled as `out.len() as isize - 1`: BYTEOUT's
    /// "increment BP then write B" is a `push`, and its carry-propagation
    /// "B = B + 1" mutates the last pushed byte in place. INITENC's
    /// "`BP = BPST − 1`" is the empty-buffer state.
    out: Vec<u8>,
}

impl MqEncoder {
    /// INITENC — initialise the encoder (T.800 §C.2.8, Figure C.10).
    ///
    /// `A = 0x8000`, `C = 0`, `CT = 12` (three spacer bits plus the byte
    /// field must fill before the first BYTEOUT), and `BP` parked before
    /// the first output byte (an empty buffer). The "preceding byte" is
    /// absent, so no spurious stuff bit and `CT` is not bumped.
    pub fn new() -> Self {
        MqEncoder {
            a: 0x8000,
            c: 0,
            ct: 12,
            out: Vec::new(),
        }
    }

    /// The byte currently under `BP` — the last completed byte, or a
    /// non-`0xFF` sentinel when the buffer is still empty (the INITENC
    /// "byte preceding `BPST`" whose value we take as not-`0xFF`).
    fn cur_byte(&self) -> u8 {
        self.out.last().copied().unwrap_or(0)
    }

    /// BYTEOUT's "increment BP, then B = value" — append a completed byte.
    fn emit(&mut self, value: u8) {
        self.out.push(value);
    }

    /// BYTEOUT's carry-propagation "B = B + 1" — add the carry into the
    /// most-recently-written byte.
    fn add_carry(&mut self) {
        if let Some(last) = self.out.last_mut() {
            *last = last.wrapping_add(1);
        }
    }

    /// BYTEOUT — remove one completed byte from `C` (T.800 §C.2.7,
    /// Figure C.9), applying the bit-stuffing and carry handling.
    fn byteout(&mut self) {
        if self.cur_byte() == 0xFF {
            // Previous byte was 0xFF: stuff a leading 0 bit, so only 7
            // data bits leave (bits 26..=20) and `CT = 7`. A carry cannot
            // reach past the 0xFF because of this stuffing, so bit 27 is
            // guaranteed clear here.
            self.emit(((self.c >> 20) & 0xFF) as u8);
            self.c &= 0xF_FFFF;
            self.ct = 7;
        } else if self.c & 0x800_0000 == 0 {
            // No carry (bit 27 clear): emit bits 26..=19 and `CT = 8`.
            self.emit(((self.c >> 19) & 0xFF) as u8);
            self.c &= 0x7_FFFF;
            self.ct = 8;
        } else {
            // Carry: propagate it into the last byte, then re-test whether
            // that byte is now 0xFF (which forces a stuff on the next).
            self.add_carry();
            if self.cur_byte() == 0xFF {
                self.c &= 0x7FF_FFFF;
                self.emit(((self.c >> 20) & 0xFF) as u8);
                self.c &= 0xF_FFFF;
                self.ct = 7;
            } else {
                self.emit(((self.c >> 19) & 0xFF) as u8);
                self.c &= 0x7_FFFF;
                self.ct = 8;
            }
        }
    }

    /// RENORME — renormalize the encoder (T.800 §C.2.6, Figure C.8).
    ///
    /// Shift `A` and `C` left one bit at a time, emitting a byte via
    /// BYTEOUT whenever `CT` reaches zero, until `A ≥ 0x8000`.
    fn renorme(&mut self) {
        loop {
            self.a <<= 1;
            self.c <<= 1;
            self.ct -= 1;
            if self.ct == 0 {
                self.byteout();
            }
            if self.a & 0x8000 != 0 {
                break;
            }
        }
    }

    /// Encode one binary decision `D` against the adaptive state in `cx`
    /// (T.800 §C.2.2 ENCODE → §C.2.3 CODE0/CODE1 → §C.2.4 CODEMPS/CODELPS).
    pub fn encode(&mut self, cx: &mut MqContext, d: u8) {
        if (d != 0) == cx.mps() {
            self.code_mps(cx);
        } else {
            self.code_lps(cx);
        }
    }

    /// CODEMPS — encode the more-probable symbol (T.800 §C.2.4,
    /// Figure C.7), with the conditional MPS/LPS exchange.
    ///
    /// The MPS occupies the upper sub-interval, so coding it adds `Qe` to
    /// `C` and reduces `A` to `A − Qe`. When that leaves `A < 0x8000` a
    /// renormalization is required and the conditional exchange applies:
    /// if the (now smaller) MPS interval `A` is below `Qe` the intervals
    /// were inverted, so `C` is left unchanged and `A = Qe`; otherwise the
    /// plain `C += Qe` holds. Either way the index advances to `NMPS`.
    fn code_mps(&mut self, cx: &mut MqContext) {
        let entry = QE[cx.index() as usize];
        let qe = entry.qe as u32;
        self.a = self.a.wrapping_sub(qe);
        if self.a & 0x8000 == 0 {
            if self.a < qe {
                // Conditional exchange: LPS interval was the larger.
                self.a = qe;
            } else {
                self.c += qe as u64;
            }
            cx.set_index(entry.nmps);
            self.renorme();
        } else {
            self.c += qe as u64;
        }
    }

    /// CODELPS — encode the less-probable symbol (T.800 §C.2.4,
    /// Figure C.6), with the conditional MPS/LPS exchange.
    ///
    /// The LPS occupies the lower sub-interval (`A = Qe`, `C` unchanged),
    /// but when the intervals are inverted (`A − Qe < Qe`) the conditional
    /// exchange codes the LPS into the upper interval instead (`C += Qe`,
    /// `A` left at `A − Qe`). The SWITCH flag may flip the MPS sense and
    /// the index advances to `NLPS`. A renormalization always follows.
    fn code_lps(&mut self, cx: &mut MqContext) {
        let entry = QE[cx.index() as usize];
        let qe = entry.qe as u32;
        self.a = self.a.wrapping_sub(qe);
        if self.a < qe {
            self.c += qe as u64;
        } else {
            self.a = qe;
        }
        if entry.switch {
            cx.flip_mps();
        }
        cx.set_index(entry.nlps);
        self.renorme();
    }

    /// SETBITS — force the low bits of `C` to `1` up to the interval bound
    /// (T.800 §C.2.9, Figure C.12).
    fn setbits(&mut self) {
        let tempc = self.c + self.a as u64;
        self.c |= 0xFFFF;
        if self.c >= tempc {
            self.c -= 0x8000;
        }
    }

    /// FLUSH — terminate the segment and return the compressed bytes
    /// (T.800 §C.2.9, Figure C.11).
    ///
    /// SETBITS packs the final data bits, two BYTEOUT shifts push the tail
    /// out, and a trailing `0xFF` byte is dropped so the `0xFF` prefix of
    /// the terminating marker overlaps the last data bits (guaranteeing any
    /// following marker is recognised). Consumes the encoder.
    pub fn flush(mut self) -> Vec<u8> {
        self.setbits();
        self.c <<= self.ct;
        self.byteout();
        self.c <<= self.ct;
        self.byteout();
        // Drop a trailing 0xFF: the terminating marker's 0xFF prefix
        // overlaps it (§C.2.9). A non-0xFF final byte is kept.
        if self.out.last() == Some(&0xFF) {
            self.out.pop();
        }
        self.out
    }
}

impl Default for MqEncoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mq::{MqContext, MqDecoder};

    /// Encode `decisions` under a fresh default context, flush, then
    /// decode the bytes back through the §C.3 decoder with the same
    /// context and assert every decision round-trips.
    fn roundtrip_default(decisions: &[u8]) {
        let mut enc = MqEncoder::new();
        let mut ecx = MqContext::default();
        for &d in decisions {
            enc.encode(&mut ecx, d);
        }
        let bytes = enc.flush();

        let mut dec = MqDecoder::new(&bytes);
        let mut dcx = MqContext::default();
        for (i, &d) in decisions.iter().enumerate() {
            let got = dec.decode(&mut dcx);
            assert_eq!(got, d, "decision {i} mismatch (len {})", decisions.len());
        }
    }

    #[test]
    fn roundtrip_all_zeros() {
        roundtrip_default(&[0u8; 500]);
    }

    #[test]
    fn roundtrip_all_ones() {
        roundtrip_default(&[1u8; 500]);
    }

    #[test]
    fn roundtrip_alternating() {
        let d: Vec<u8> = (0..1000).map(|i| (i % 2) as u8).collect();
        roundtrip_default(&d);
    }

    #[test]
    fn roundtrip_pseudo_random() {
        // A cheap LCG so the test needs no external rng crate.
        let mut state: u32 = 0x1234_5678;
        let mut d = Vec::with_capacity(5000);
        for _ in 0..5000 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            d.push(((state >> 16) & 1) as u8);
        }
        roundtrip_default(&d);
    }

    #[test]
    fn roundtrip_mostly_ones_carry_heavy() {
        // A long run biased hard toward 1 exercises carry propagation and
        // the 0xFF bit-stuffing in BYTEOUT.
        let mut state: u32 = 0xDEAD_BEEF;
        let mut d = Vec::with_capacity(8000);
        for _ in 0..8000 {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            // ~7/8 ones.
            d.push(if (state >> 13) & 7 == 0 { 0 } else { 1 });
        }
        roundtrip_default(&d);
    }

    #[test]
    fn roundtrip_multi_context() {
        // Several independent contexts interleaved, mirroring how the
        // Annex D passes drive many CX labels through one encoder.
        let mut enc = MqEncoder::new();
        let mut ecx = [
            MqContext::default(),
            MqContext::uniform(),
            MqContext::run_length(),
            MqContext::zero_neighbours(),
        ];
        let mut state: u32 = 0x0BAD_F00D;
        let mut plan = Vec::with_capacity(4000);
        for _ in 0..4000 {
            state = state.wrapping_mul(22_695_477).wrapping_add(1);
            let ctx = ((state >> 28) & 3) as usize;
            let bit = ((state >> 15) & 1) as u8;
            plan.push((ctx, bit));
            enc.encode(&mut ecx[ctx], bit);
        }
        let bytes = enc.flush();

        let mut dec = MqDecoder::new(&bytes);
        let mut dcx = [
            MqContext::default(),
            MqContext::uniform(),
            MqContext::run_length(),
            MqContext::zero_neighbours(),
        ];
        for (i, &(ctx, bit)) in plan.iter().enumerate() {
            assert_eq!(dec.decode(&mut dcx[ctx]), bit, "step {i}");
        }
    }

    #[test]
    fn empty_flush_is_decodable() {
        // No decisions: FLUSH still yields a valid (possibly empty)
        // segment the decoder opens without panicking.
        let enc = MqEncoder::new();
        let bytes = enc.flush();
        let mut dec = MqDecoder::new(&bytes);
        let mut cx = MqContext::default();
        // Decoding past the end must not panic (it yields the residual MPS
        // run per §C.3.4 / §D.4.1).
        for _ in 0..16 {
            let _ = dec.decode(&mut cx);
        }
    }

    #[test]
    fn flush_never_ends_on_0xff() {
        // The §C.2.9 trailing-0xFF discard guarantees the terminated
        // segment never ends on a 0xFF (which would collide with a marker
        // prefix).
        let mut state: u32 = 0x5555_AAAA;
        for _ in 0..200 {
            let mut enc = MqEncoder::new();
            let mut cx = MqContext::default();
            let mut d = Vec::new();
            for _ in 0..300 {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                d.push(((state >> 17) & 1) as u8);
            }
            for &b in &d {
                enc.encode(&mut cx, b);
            }
            let bytes = enc.flush();
            assert_ne!(bytes.last(), Some(&0xFF));
        }
    }
}
