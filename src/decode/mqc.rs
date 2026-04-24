//! MQ arithmetic decoder (ISO/IEC 15444-1 Annex C).
//!
//! Port of the OpenJPEG `mqc.c` decoder state machine (BSD-2-Clause).
//! The 47-state probability table, the `BYTEIN`, `RENORMD` and `DECODE`
//! primitives, and the state transition on MPS/LPS are translated
//! directly. Only the decoder side is needed — JPEG 2000 encode is not
//! implemented in this crate.
//!
//! Two operating modes are supported:
//! - **Arithmetic** (`init_dec`): the default adaptive binary arithmetic
//!   coder used by the cleanup / significance-propagation / magnitude-
//!   refinement passes.
//! - **Raw (bypass)** (`raw_init_dec`): plain bit-unpacking with the
//!   marker-avoidance unstuffing rule used by the "arithmetic coder
//!   bypass" (`SELECTIVE_ARITHMETIC_CODING_BYPASS`) mode switch.

/// Number of EBCOT tier-1 MQ contexts (ISO/IEC 15444-1 Table D-1).
pub const NUM_CTX: usize = 19;

/// Index of the UNIFORM context (used for raw sign bits in cleanup pass
/// runs).
pub const CTX_UNI: usize = 17;
/// Index of the RUN / AGG context (used for the 4-bit run length in
/// cleanup pass).
pub const CTX_AGG: usize = 18;
/// Index of the ZC (zero coding) contexts — 9 slots (0..=8).
pub const CTX_ZC: usize = 0;
/// Index of the SC (sign coding) contexts — 5 slots (9..=13).
pub const CTX_SC: usize = 9;
/// Index of the MAG (magnitude refinement) contexts — 3 slots (14..=16).
pub const CTX_MAG: usize = 14;

/// Extra bytes the decoder may need to read past the declared code-block
/// end. `init_dec` writes two 0xFF sentinel bytes there and the macros
/// walk one byte ahead of `bp`, so two spare bytes is always enough.
pub const CBLK_DATA_EXTRA: usize = 2;

/// One entry in the 47-state probability estimation table.
#[derive(Clone, Copy)]
struct State {
    /// Lower-bound probability estimate for the LPS (Qe in spec).
    qeval: u32,
    /// Most probable symbol (0 or 1).
    mps: u8,
    /// Next state on LPS renormalization (Table C.2 NLPS column).
    nlps: u16,
    /// Next state on MPS renormalization (Table C.2 NMPS column).
    nmps: u16,
}

/// The 47-state table. Indexes `[state*2 + msb]` — the `msb` bit is
/// baked into the state index so both members of a state pair live
/// adjacent in the flat array.
const STATES: [State; 94] = [
    State {
        qeval: 0x5601,
        mps: 0,
        nlps: 3,
        nmps: 2,
    },
    State {
        qeval: 0x5601,
        mps: 1,
        nlps: 2,
        nmps: 3,
    },
    State {
        qeval: 0x3401,
        mps: 0,
        nlps: 12,
        nmps: 4,
    },
    State {
        qeval: 0x3401,
        mps: 1,
        nlps: 13,
        nmps: 5,
    },
    State {
        qeval: 0x1801,
        mps: 0,
        nlps: 18,
        nmps: 6,
    },
    State {
        qeval: 0x1801,
        mps: 1,
        nlps: 19,
        nmps: 7,
    },
    State {
        qeval: 0x0ac1,
        mps: 0,
        nlps: 24,
        nmps: 8,
    },
    State {
        qeval: 0x0ac1,
        mps: 1,
        nlps: 25,
        nmps: 9,
    },
    State {
        qeval: 0x0521,
        mps: 0,
        nlps: 58,
        nmps: 10,
    },
    State {
        qeval: 0x0521,
        mps: 1,
        nlps: 59,
        nmps: 11,
    },
    State {
        qeval: 0x0221,
        mps: 0,
        nlps: 66,
        nmps: 76,
    },
    State {
        qeval: 0x0221,
        mps: 1,
        nlps: 67,
        nmps: 77,
    },
    State {
        qeval: 0x5601,
        mps: 0,
        nlps: 13,
        nmps: 14,
    },
    State {
        qeval: 0x5601,
        mps: 1,
        nlps: 12,
        nmps: 15,
    },
    State {
        qeval: 0x5401,
        mps: 0,
        nlps: 28,
        nmps: 16,
    },
    State {
        qeval: 0x5401,
        mps: 1,
        nlps: 29,
        nmps: 17,
    },
    State {
        qeval: 0x4801,
        mps: 0,
        nlps: 28,
        nmps: 18,
    },
    State {
        qeval: 0x4801,
        mps: 1,
        nlps: 29,
        nmps: 19,
    },
    State {
        qeval: 0x3801,
        mps: 0,
        nlps: 28,
        nmps: 20,
    },
    State {
        qeval: 0x3801,
        mps: 1,
        nlps: 29,
        nmps: 21,
    },
    State {
        qeval: 0x3001,
        mps: 0,
        nlps: 34,
        nmps: 22,
    },
    State {
        qeval: 0x3001,
        mps: 1,
        nlps: 35,
        nmps: 23,
    },
    State {
        qeval: 0x2401,
        mps: 0,
        nlps: 36,
        nmps: 24,
    },
    State {
        qeval: 0x2401,
        mps: 1,
        nlps: 37,
        nmps: 25,
    },
    State {
        qeval: 0x1c01,
        mps: 0,
        nlps: 40,
        nmps: 26,
    },
    State {
        qeval: 0x1c01,
        mps: 1,
        nlps: 41,
        nmps: 27,
    },
    State {
        qeval: 0x1601,
        mps: 0,
        nlps: 42,
        nmps: 58,
    },
    State {
        qeval: 0x1601,
        mps: 1,
        nlps: 43,
        nmps: 59,
    },
    State {
        qeval: 0x5601,
        mps: 0,
        nlps: 29,
        nmps: 30,
    },
    State {
        qeval: 0x5601,
        mps: 1,
        nlps: 28,
        nmps: 31,
    },
    State {
        qeval: 0x5401,
        mps: 0,
        nlps: 28,
        nmps: 32,
    },
    State {
        qeval: 0x5401,
        mps: 1,
        nlps: 29,
        nmps: 33,
    },
    State {
        qeval: 0x5101,
        mps: 0,
        nlps: 30,
        nmps: 34,
    },
    State {
        qeval: 0x5101,
        mps: 1,
        nlps: 31,
        nmps: 35,
    },
    State {
        qeval: 0x4801,
        mps: 0,
        nlps: 32,
        nmps: 36,
    },
    State {
        qeval: 0x4801,
        mps: 1,
        nlps: 33,
        nmps: 37,
    },
    State {
        qeval: 0x3801,
        mps: 0,
        nlps: 34,
        nmps: 38,
    },
    State {
        qeval: 0x3801,
        mps: 1,
        nlps: 35,
        nmps: 39,
    },
    State {
        qeval: 0x3401,
        mps: 0,
        nlps: 36,
        nmps: 40,
    },
    State {
        qeval: 0x3401,
        mps: 1,
        nlps: 37,
        nmps: 41,
    },
    State {
        qeval: 0x3001,
        mps: 0,
        nlps: 38,
        nmps: 42,
    },
    State {
        qeval: 0x3001,
        mps: 1,
        nlps: 39,
        nmps: 43,
    },
    State {
        qeval: 0x2801,
        mps: 0,
        nlps: 38,
        nmps: 44,
    },
    State {
        qeval: 0x2801,
        mps: 1,
        nlps: 39,
        nmps: 45,
    },
    State {
        qeval: 0x2401,
        mps: 0,
        nlps: 40,
        nmps: 46,
    },
    State {
        qeval: 0x2401,
        mps: 1,
        nlps: 41,
        nmps: 47,
    },
    State {
        qeval: 0x2201,
        mps: 0,
        nlps: 42,
        nmps: 48,
    },
    State {
        qeval: 0x2201,
        mps: 1,
        nlps: 43,
        nmps: 49,
    },
    State {
        qeval: 0x1c01,
        mps: 0,
        nlps: 44,
        nmps: 50,
    },
    State {
        qeval: 0x1c01,
        mps: 1,
        nlps: 45,
        nmps: 51,
    },
    State {
        qeval: 0x1801,
        mps: 0,
        nlps: 46,
        nmps: 52,
    },
    State {
        qeval: 0x1801,
        mps: 1,
        nlps: 47,
        nmps: 53,
    },
    State {
        qeval: 0x1601,
        mps: 0,
        nlps: 48,
        nmps: 54,
    },
    State {
        qeval: 0x1601,
        mps: 1,
        nlps: 49,
        nmps: 55,
    },
    State {
        qeval: 0x1401,
        mps: 0,
        nlps: 50,
        nmps: 56,
    },
    State {
        qeval: 0x1401,
        mps: 1,
        nlps: 51,
        nmps: 57,
    },
    State {
        qeval: 0x1201,
        mps: 0,
        nlps: 52,
        nmps: 58,
    },
    State {
        qeval: 0x1201,
        mps: 1,
        nlps: 53,
        nmps: 59,
    },
    State {
        qeval: 0x1101,
        mps: 0,
        nlps: 54,
        nmps: 60,
    },
    State {
        qeval: 0x1101,
        mps: 1,
        nlps: 55,
        nmps: 61,
    },
    State {
        qeval: 0x0ac1,
        mps: 0,
        nlps: 56,
        nmps: 62,
    },
    State {
        qeval: 0x0ac1,
        mps: 1,
        nlps: 57,
        nmps: 63,
    },
    State {
        qeval: 0x09c1,
        mps: 0,
        nlps: 58,
        nmps: 64,
    },
    State {
        qeval: 0x09c1,
        mps: 1,
        nlps: 59,
        nmps: 65,
    },
    State {
        qeval: 0x08a1,
        mps: 0,
        nlps: 60,
        nmps: 66,
    },
    State {
        qeval: 0x08a1,
        mps: 1,
        nlps: 61,
        nmps: 67,
    },
    State {
        qeval: 0x0521,
        mps: 0,
        nlps: 62,
        nmps: 68,
    },
    State {
        qeval: 0x0521,
        mps: 1,
        nlps: 63,
        nmps: 69,
    },
    State {
        qeval: 0x0441,
        mps: 0,
        nlps: 64,
        nmps: 70,
    },
    State {
        qeval: 0x0441,
        mps: 1,
        nlps: 65,
        nmps: 71,
    },
    State {
        qeval: 0x02a1,
        mps: 0,
        nlps: 66,
        nmps: 72,
    },
    State {
        qeval: 0x02a1,
        mps: 1,
        nlps: 67,
        nmps: 73,
    },
    State {
        qeval: 0x0221,
        mps: 0,
        nlps: 68,
        nmps: 74,
    },
    State {
        qeval: 0x0221,
        mps: 1,
        nlps: 69,
        nmps: 75,
    },
    State {
        qeval: 0x0141,
        mps: 0,
        nlps: 70,
        nmps: 76,
    },
    State {
        qeval: 0x0141,
        mps: 1,
        nlps: 71,
        nmps: 77,
    },
    State {
        qeval: 0x0111,
        mps: 0,
        nlps: 72,
        nmps: 78,
    },
    State {
        qeval: 0x0111,
        mps: 1,
        nlps: 73,
        nmps: 79,
    },
    State {
        qeval: 0x0085,
        mps: 0,
        nlps: 74,
        nmps: 80,
    },
    State {
        qeval: 0x0085,
        mps: 1,
        nlps: 75,
        nmps: 81,
    },
    State {
        qeval: 0x0049,
        mps: 0,
        nlps: 76,
        nmps: 82,
    },
    State {
        qeval: 0x0049,
        mps: 1,
        nlps: 77,
        nmps: 83,
    },
    State {
        qeval: 0x0025,
        mps: 0,
        nlps: 78,
        nmps: 84,
    },
    State {
        qeval: 0x0025,
        mps: 1,
        nlps: 79,
        nmps: 85,
    },
    State {
        qeval: 0x0015,
        mps: 0,
        nlps: 80,
        nmps: 86,
    },
    State {
        qeval: 0x0015,
        mps: 1,
        nlps: 81,
        nmps: 87,
    },
    State {
        qeval: 0x0009,
        mps: 0,
        nlps: 82,
        nmps: 88,
    },
    State {
        qeval: 0x0009,
        mps: 1,
        nlps: 83,
        nmps: 89,
    },
    State {
        qeval: 0x0005,
        mps: 0,
        nlps: 84,
        nmps: 90,
    },
    State {
        qeval: 0x0005,
        mps: 1,
        nlps: 85,
        nmps: 91,
    },
    State {
        qeval: 0x0001,
        mps: 0,
        nlps: 86,
        nmps: 90,
    },
    State {
        qeval: 0x0001,
        mps: 1,
        nlps: 87,
        nmps: 91,
    },
    State {
        qeval: 0x5601,
        mps: 0,
        nlps: 92,
        nmps: 92,
    },
    State {
        qeval: 0x5601,
        mps: 1,
        nlps: 93,
        nmps: 93,
    },
];

/// MQ arithmetic decoder state.
pub struct Mqc {
    /// Full compressed buffer for the block. Two 0xFF sentinel bytes are
    /// appended inside `init_dec` so lookahead never reads past the end.
    data: Vec<u8>,
    /// Byte pointer (index into `data`).
    bp: usize,
    /// Arithmetic range register (`A`).
    a: u32,
    /// Code register (`C`).
    c: u32,
    /// Count of usable bits in C's high byte.
    ct: u32,
    /// Per-context state pointers — index into STATES.
    ctxs: [u16; NUM_CTX],
    /// Currently selected context (index into `ctxs`).
    curctx: usize,
}

impl Mqc {
    /// Initialise the arithmetic decoder on the given code-block data.
    /// The stream is cloned so we can append sentinel bytes without
    /// mutating the caller's buffer.
    pub fn init_dec(mut data: Vec<u8>) -> Self {
        let original_len = data.len();
        // Append two 0xFF sentinel bytes. These mimic an all-ones EOF
        // marker that opj_mqc_bytein stops on.
        data.push(0xFF);
        data.push(0xFF);
        let mut m = Mqc {
            data,
            bp: 0,
            a: 0,
            c: 0,
            ct: 0,
            ctxs: [0; NUM_CTX],
            curctx: 0,
        };
        m.c = if original_len == 0 {
            0xFFu32 << 16
        } else {
            (m.data[0] as u32) << 16
        };
        m.bytein();
        m.c <<= 7;
        m.ct -= 7;
        m.a = 0x8000;
        m
    }

    /// Initialise the raw (bypass) decoder on the given code-block data.
    pub fn raw_init_dec(mut data: Vec<u8>) -> Self {
        data.push(0xFF);
        data.push(0xFF);
        Mqc {
            data,
            bp: 0,
            a: 0,
            c: 0,
            ct: 0,
            ctxs: [0; NUM_CTX],
            curctx: 0,
        }
    }

    /// Reset all 19 tier-1 contexts to their initial state values.
    /// Mirrors `opj_mqc_resetstates` + the tier-1 overrides for UNI, AGG
    /// and ZC.
    pub fn resetstates(&mut self) {
        for c in &mut self.ctxs {
            *c = 0;
        }
        // Overrides from opj_t1_init_ctxno_zc / opj_mqc_reset_enc.
        self.setstate(CTX_UNI, 0, 46);
        self.setstate(CTX_AGG, 0, 3);
        self.setstate(CTX_ZC, 0, 4);
    }

    /// Set a specific context to a given state (mps + probability row).
    pub fn setstate(&mut self, ctxno: usize, msb: u32, prob: u32) {
        self.ctxs[ctxno] = (msb + (prob << 1)) as u16;
    }

    /// Select a context for the following `decode` calls.
    #[inline]
    pub fn setcurctx(&mut self, ctxno: usize) {
        self.curctx = ctxno;
    }

    /// Arithmetic decode: consume one binary symbol using the current
    /// context. Returns 0 or 1.
    #[inline]
    pub fn decode(&mut self) -> u32 {
        // Implements ISO 15444-1 C.3.2 DECODE.
        let state_ix = self.ctxs[self.curctx] as usize;
        let st = &STATES[state_ix];
        let qeval = st.qeval;
        self.a -= qeval;
        let d;
        if (self.c >> 16) < qeval {
            d = self.lps_exchange(qeval);
            self.renormd();
        } else {
            self.c -= qeval << 16;
            if (self.a & 0x8000) == 0 {
                d = self.mps_exchange(qeval);
                self.renormd();
            } else {
                d = STATES[self.ctxs[self.curctx] as usize].mps as u32;
            }
        }
        d
    }

    /// Raw bypass decode: consume the next pre-stored bit (MSB-first)
    /// from the compressed stream, honouring the 0xFF-prefix unstuffing
    /// rule.
    #[inline]
    pub fn raw_decode(&mut self) -> u32 {
        if self.ct == 0 {
            if self.c == 0xFF {
                if self.current_byte() > 0x8F {
                    self.c = 0xFF;
                    self.ct = 8;
                } else {
                    self.c = self.current_byte() as u32;
                    self.bp += 1;
                    self.ct = 7;
                }
            } else {
                self.c = self.current_byte() as u32;
                self.bp += 1;
                self.ct = 8;
            }
        }
        self.ct -= 1;
        (self.c >> self.ct) & 1
    }

    #[inline]
    fn current_byte(&self) -> u8 {
        // `data` always has two 0xFF sentinels appended.
        self.data[self.bp]
    }

    #[inline]
    fn lps_exchange(&mut self, qeval: u32) -> u32 {
        let state_ix = self.ctxs[self.curctx] as usize;
        let st = &STATES[state_ix];
        let d;
        if self.a < qeval {
            self.a = qeval;
            d = st.mps as u32;
            self.ctxs[self.curctx] = st.nmps;
        } else {
            self.a = qeval;
            d = (st.mps ^ 1) as u32;
            self.ctxs[self.curctx] = st.nlps;
        }
        d
    }

    #[inline]
    fn mps_exchange(&mut self, qeval: u32) -> u32 {
        let state_ix = self.ctxs[self.curctx] as usize;
        let st = &STATES[state_ix];
        let d;
        if self.a < qeval {
            d = (st.mps ^ 1) as u32;
            self.ctxs[self.curctx] = st.nlps;
        } else {
            d = st.mps as u32;
            self.ctxs[self.curctx] = st.nmps;
        }
        d
    }

    #[inline]
    fn renormd(&mut self) {
        loop {
            if self.ct == 0 {
                self.bytein();
            }
            self.a <<= 1;
            self.c <<= 1;
            self.ct -= 1;
            if self.a >= 0x8000 {
                break;
            }
        }
    }

    #[inline]
    fn bytein(&mut self) {
        // Peek at bp and bp+1; both are guaranteed readable thanks to
        // the trailing 0xFF sentinels.
        let cur = self.data[self.bp];
        let nxt = self.data[self.bp + 1];
        if cur == 0xFF {
            if nxt > 0x8F {
                self.c = self.c.wrapping_add(0xFF00);
                self.ct = 8;
                // end_of_byte_stream_counter: used by OpenJPEG for the
                // MQ-flush / termination logic, which we don't rely on.
            } else {
                self.bp += 1;
                self.c = self.c.wrapping_add((nxt as u32) << 9);
                self.ct = 7;
            }
        } else {
            self.bp += 1;
            self.c = self.c.wrapping_add((nxt as u32) << 8);
            self.ct = 8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_table_has_94_entries() {
        assert_eq!(STATES.len(), 94);
    }

    #[test]
    fn init_dec_sets_initial_range() {
        // Any plausible code-block bytes will do.
        let data = vec![0x00, 0x00, 0x00, 0x00];
        let m = Mqc::init_dec(data);
        assert_eq!(m.a, 0x8000);
    }

    #[test]
    fn reset_then_decode_zero_state() {
        // Smoke test: decoding a near-all-zeros byte stream at the default
        // ZC state should not panic. Bit counts vary with the Qe state
        // transitions, so we don't assert a specific split here — just
        // that the coder makes forward progress.
        let data = vec![0x00, 0x00, 0x00, 0x00];
        let mut m = Mqc::init_dec(data);
        m.resetstates();
        m.setcurctx(0);
        for _ in 0..16 {
            let _ = m.decode();
        }
    }

    #[test]
    fn raw_decode_extracts_msb_first_bits() {
        // Not a real bypass stream, but bit extraction should be MSB-first
        // for plain bytes (no 0xFF stuffing to worry about).
        let data = vec![0b1010_1100, 0b1111_0000];
        let mut m = Mqc::raw_init_dec(data);
        let bits: Vec<u32> = (0..8).map(|_| m.raw_decode()).collect();
        assert_eq!(bits, vec![1, 0, 1, 0, 1, 1, 0, 0]);
    }
}
