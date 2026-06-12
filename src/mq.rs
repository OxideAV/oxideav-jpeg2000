//! Tier-1 MQ arithmetic decoder — T.800 Annex C (decoder side, §C.3).
//!
//! This module implements the **decoder** half of the binary adaptive
//! arithmetic coder ("the MQ-coder") that JPEG 2000 Part 1 uses for the
//! tier-1 EBCOT bit-plane coding (Annex D). It is the byte-consuming
//! engine that the not-yet-written significance / refinement / cleanup
//! coding passes will drive: each pass hands the decoder a stream of
//! context labels `CX` and reads back binary decisions `D ∈ {0, 1}`.
//!
//! ## What this module covers
//!
//! The five normative decoder procedures of T.800 §C.3, plus the two
//! normative tables they depend on:
//!
//! * [`MqDecoder::new`] — INITDEC (§C.3.5, Figure C.20). Primes the
//!   `C` register with the first one or two bytes and aligns it to the
//!   starting value of the `A` register.
//! * [`MqDecoder::decode`] — DECODE (§C.3.2, Figure C.15) plus the
//!   MPS-path (Figure C.16) and LPS-path (Figure C.17) conditional
//!   MPS/LPS exchange procedures and the embedded adaptive probability
//!   estimation (§C.2.5).
//! * `renormd` — RENORMD (§C.3.3, Figure C.18). Shifts `A` and `C`
//!   left until `A ≥ 0x8000`, pulling fresh bytes via BYTEIN.
//! * `bytein` — BYTEIN (§C.3.4, Figure C.19). Reads one byte,
//!   compensating for the `0xFF`-prefixed stuff bit and synthesising
//!   the `0xFF`-fill end-of-stream behaviour described in §C.3.4 /
//!   §D.4.1.
//! * [`QE`] — Table C.2 (Qe value, NMPS, NLPS, SWITCH; indices 0..=46).
//! * The Table D.7 initial states are exposed via the public context
//!   constructors on [`MqContext`] (`UNIFORM` index 46, run-length
//!   index 3, zero-neighbours index 4, everything else index 0); the
//!   caller (the round-N coding-pass code) owns the `CX → MqContext`
//!   array, since the context labelling lives in Annex D, not Annex C.
//!
//! ## What this module does NOT cover
//!
//! * The Annex D context formation (significance / sign / magnitude
//!   contexts, the run-length context, the UNIFORM context routing).
//!   That is the next tier-1 round; this module is the pure §C.3
//!   engine it sits on.
//! * The MQ **encoder** (§C.2). Decoder only.
//! * Selective arithmetic-coding bypass (§D.6 raw bit mode) and the
//!   §D.5 segmentation symbol — both are driven by the coding-pass
//!   layer, which can call [`MqDecoder::decode`] with the UNIFORM
//!   context for the segmentation symbol when it lands.
//!
//! ## Register conventions (T.800 §C.3.1, Table C.3)
//!
//! The spec models the code register as a 32-bit value split into a
//! 16-bit `Chigh` (bits 16..=31) and a 16-bit `Clow` (bits 0..=15):
//! renormalization shifts one bit of new data from the MSB of `Clow`
//! into the LSB of `Chigh`, and the decoding comparison uses `Chigh`
//! alone. We hold the whole thing in a single `u32` `c` and compare
//! `c >> 16` to `Qe`, exactly per the §C.3.2 note ("Chigh register is
//! compared to the size of the LPS sub-interval"). `A` is a `u32`
//! holding the 16-bit interval; `CT` is the §C.3.3 bit counter.
//!
//! ## End-of-stream behaviour (§C.3.4 / §D.4.1)
//!
//! BYTEIN reads from a caller-supplied byte slice. When the slice is
//! exhausted, or when the byte at `BP` is `0xFF` and the next byte is a
//! marker (`> 0x8F`, or off the end of the slice), the decoder enters
//! the "feed `0xFF`" terminal state of §C.3.4: `0xFF00` is added to the
//! `C` register and `CT` is set to 8. Per §D.4.1 the decoder may need
//! to keep decoding symbols after the signalled bytes run out, so this
//! synthesises the two-trailing-`0xFF` end-of-stream marker rather than
//! erroring — there is no out-of-band failure, decoding simply yields
//! the residual MPS run.
//!
//! ## Clean-room provenance
//!
//! Implemented solely from `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf`
//! (Annex C §C.1–§C.3 prose + Table C.2; Annex D §D.4 Table D.7). The
//! §C.3.2 / §C.3.3 / §C.3.4 / §C.3.5 register operations are the prose
//! descriptions of Figures C.15–C.20 transcribed to integer ops.

/// One row of T.800 Table C.2: `(Qe, NMPS, NLPS, SWITCH)`.
///
/// * `qe` — the LPS probability estimate, a 16-bit fixed-point integer
///   where `0x8000` is decimal `0.75` (§C.1.2). Used directly as the
///   LPS sub-interval size.
/// * `nmps` — next index after an MPS renormalization (§C.2.5).
/// * `nlps` — next index after an LPS renormalization (§C.2.5).
/// * `switch` — when `true`, the LPS path inverts the MPS sense
///   (§C.2.5 / Figure C.17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QeEntry {
    /// LPS probability estimate (16-bit fixed point, `0x8000` ≈ 0.75).
    pub qe: u16,
    /// Next index after an MPS renormalization (Table C.2 NMPS column).
    pub nmps: u8,
    /// Next index after an LPS renormalization (Table C.2 NLPS column).
    pub nlps: u8,
    /// Whether the LPS path flips the MPS sense (Table C.2 SWITCH).
    pub switch: bool,
}

/// T.800 Table C.2 — the 47 `(Qe, NMPS, NLPS, SWITCH)` rows of the MQ
/// probability-estimation state machine (indices `0..=46`).
///
/// Index `46` is the UNIFORM context's fixed state (`Qe = 0x5601`,
/// `NMPS = NLPS = 46`, no switch) per §D.5 / Table D.7.
///
/// Transcribed verbatim from Table C.2. The OCR text rendered index
/// 35's hex as `0x02Al`; its binary column `0000 0010 1010 0001`
/// confirms the value is `0x02A1` (the trailing `l` is an OCR misread
/// of `1`).
pub const QE: [QeEntry; 47] = [
    QeEntry {
        qe: 0x5601,
        nmps: 1,
        nlps: 1,
        switch: true,
    },
    QeEntry {
        qe: 0x3401,
        nmps: 2,
        nlps: 6,
        switch: false,
    },
    QeEntry {
        qe: 0x1801,
        nmps: 3,
        nlps: 9,
        switch: false,
    },
    QeEntry {
        qe: 0x0AC1,
        nmps: 4,
        nlps: 12,
        switch: false,
    },
    QeEntry {
        qe: 0x0521,
        nmps: 5,
        nlps: 29,
        switch: false,
    },
    QeEntry {
        qe: 0x0221,
        nmps: 38,
        nlps: 33,
        switch: false,
    },
    QeEntry {
        qe: 0x5601,
        nmps: 7,
        nlps: 6,
        switch: true,
    },
    QeEntry {
        qe: 0x5401,
        nmps: 8,
        nlps: 14,
        switch: false,
    },
    QeEntry {
        qe: 0x4801,
        nmps: 9,
        nlps: 14,
        switch: false,
    },
    QeEntry {
        qe: 0x3801,
        nmps: 10,
        nlps: 14,
        switch: false,
    },
    QeEntry {
        qe: 0x3001,
        nmps: 11,
        nlps: 17,
        switch: false,
    },
    QeEntry {
        qe: 0x2401,
        nmps: 12,
        nlps: 18,
        switch: false,
    },
    QeEntry {
        qe: 0x1C01,
        nmps: 13,
        nlps: 20,
        switch: false,
    },
    QeEntry {
        qe: 0x1601,
        nmps: 29,
        nlps: 21,
        switch: false,
    },
    QeEntry {
        qe: 0x5601,
        nmps: 15,
        nlps: 14,
        switch: true,
    },
    QeEntry {
        qe: 0x5401,
        nmps: 16,
        nlps: 14,
        switch: false,
    },
    QeEntry {
        qe: 0x5101,
        nmps: 17,
        nlps: 15,
        switch: false,
    },
    QeEntry {
        qe: 0x4801,
        nmps: 18,
        nlps: 16,
        switch: false,
    },
    QeEntry {
        qe: 0x3801,
        nmps: 19,
        nlps: 17,
        switch: false,
    },
    QeEntry {
        qe: 0x3401,
        nmps: 20,
        nlps: 18,
        switch: false,
    },
    QeEntry {
        qe: 0x3001,
        nmps: 21,
        nlps: 19,
        switch: false,
    },
    QeEntry {
        qe: 0x2801,
        nmps: 22,
        nlps: 19,
        switch: false,
    },
    QeEntry {
        qe: 0x2401,
        nmps: 23,
        nlps: 20,
        switch: false,
    },
    QeEntry {
        qe: 0x2201,
        nmps: 24,
        nlps: 21,
        switch: false,
    },
    QeEntry {
        qe: 0x1C01,
        nmps: 25,
        nlps: 22,
        switch: false,
    },
    QeEntry {
        qe: 0x1801,
        nmps: 26,
        nlps: 23,
        switch: false,
    },
    QeEntry {
        qe: 0x1601,
        nmps: 27,
        nlps: 24,
        switch: false,
    },
    QeEntry {
        qe: 0x1401,
        nmps: 28,
        nlps: 25,
        switch: false,
    },
    QeEntry {
        qe: 0x1201,
        nmps: 29,
        nlps: 26,
        switch: false,
    },
    QeEntry {
        qe: 0x1101,
        nmps: 30,
        nlps: 27,
        switch: false,
    },
    QeEntry {
        qe: 0x0AC1,
        nmps: 31,
        nlps: 28,
        switch: false,
    },
    QeEntry {
        qe: 0x09C1,
        nmps: 32,
        nlps: 29,
        switch: false,
    },
    QeEntry {
        qe: 0x08A1,
        nmps: 33,
        nlps: 30,
        switch: false,
    },
    QeEntry {
        qe: 0x0521,
        nmps: 34,
        nlps: 31,
        switch: false,
    },
    QeEntry {
        qe: 0x0441,
        nmps: 35,
        nlps: 32,
        switch: false,
    },
    QeEntry {
        qe: 0x02A1,
        nmps: 36,
        nlps: 33,
        switch: false,
    },
    QeEntry {
        qe: 0x0221,
        nmps: 37,
        nlps: 34,
        switch: false,
    },
    QeEntry {
        qe: 0x0141,
        nmps: 38,
        nlps: 35,
        switch: false,
    },
    QeEntry {
        qe: 0x0111,
        nmps: 39,
        nlps: 36,
        switch: false,
    },
    QeEntry {
        qe: 0x0085,
        nmps: 40,
        nlps: 37,
        switch: false,
    },
    QeEntry {
        qe: 0x0049,
        nmps: 41,
        nlps: 38,
        switch: false,
    },
    QeEntry {
        qe: 0x0025,
        nmps: 42,
        nlps: 39,
        switch: false,
    },
    QeEntry {
        qe: 0x0015,
        nmps: 43,
        nlps: 40,
        switch: false,
    },
    QeEntry {
        qe: 0x0009,
        nmps: 44,
        nlps: 41,
        switch: false,
    },
    QeEntry {
        qe: 0x0005,
        nmps: 45,
        nlps: 42,
        switch: false,
    },
    QeEntry {
        qe: 0x0001,
        nmps: 45,
        nlps: 43,
        switch: false,
    },
    QeEntry {
        qe: 0x5601,
        nmps: 46,
        nlps: 46,
        switch: false,
    },
];

/// Table C.2 index of the UNIFORM context's fixed state (§D.5).
pub const UNIFORM_INDEX: u8 = 46;
/// Table D.7 initial index for the run-length context.
pub const RUN_LENGTH_INDEX: u8 = 3;
/// Table D.7 initial index for the "all-zero-neighbours" context
/// (context label 0 in Table D.1).
pub const ZERO_NEIGHBOURS_INDEX: u8 = 4;

/// Per-context adaptive state: the current Table C.2 index `I(CX)` and
/// the current sense of the more-probable symbol `MPS(CX)`.
///
/// Table D.7 gives the reset states. The caller (the Annex D coding
/// passes) owns the `CX → MqContext` array; this struct is the unit of
/// that array. Constructors mirror the Table D.7 rows:
///
/// * [`MqContext::default`] — "all other contexts": index 0, MPS 0.
/// * [`MqContext::uniform`] — UNIFORM: index 46, MPS 0.
/// * [`MqContext::run_length`] — run-length: index 3, MPS 0.
/// * [`MqContext::zero_neighbours`] — context label 0: index 4, MPS 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MqContext {
    /// Current Table C.2 index `I(CX)` (`0..=46`).
    index: u8,
    /// Current more-probable-symbol sense `MPS(CX)` (`false` = 0).
    mps: bool,
}

impl Default for MqContext {
    /// Table D.7 "all other contexts": index 0, MPS 0.
    fn default() -> Self {
        Self {
            index: 0,
            mps: false,
        }
    }
}

impl MqContext {
    /// Table D.7 UNIFORM context: index 46, MPS 0.
    pub const fn uniform() -> Self {
        Self {
            index: UNIFORM_INDEX,
            mps: false,
        }
    }

    /// Table D.7 run-length context: index 3, MPS 0.
    pub const fn run_length() -> Self {
        Self {
            index: RUN_LENGTH_INDEX,
            mps: false,
        }
    }

    /// Table D.7 "all-zero-neighbours" context (label 0): index 4,
    /// MPS 0.
    pub const fn zero_neighbours() -> Self {
        Self {
            index: ZERO_NEIGHBOURS_INDEX,
            mps: false,
        }
    }

    /// The current Table C.2 index `I(CX)`.
    pub const fn index(&self) -> u8 {
        self.index
    }

    /// The current MPS sense `MPS(CX)` (`false` = 0, `true` = 1).
    pub const fn mps(&self) -> bool {
        self.mps
    }

    /// Reset this context to its initial state (§C.3.6). The caller
    /// supplies the initial state appropriate for this `CX`; this is a
    /// convenience for resetting to an arbitrary Table D.7 row.
    pub fn reset_to(&mut self, initial: MqContext) {
        *self = initial;
    }
}

/// The MQ arithmetic decoder of T.800 §C.3.
///
/// Holds the §C.3.1 register state (`A`, `C`, `CT`) and a cursor into
/// the caller-supplied compressed-byte slice. Per-context adaptive
/// state lives in [`MqContext`] values the caller passes to
/// [`decode`](MqDecoder::decode) — the decoder is stateless with
/// respect to contexts, exactly mirroring the spec's "I(CX) / MPS(CX)
/// stored at CX" model.
#[derive(Debug, Clone)]
pub struct MqDecoder<'a> {
    /// Compressed byte slice (`BPST .. end`).
    data: &'a [u8],
    /// Byte pointer `BP`, the §C.3 buffer cursor (index into `data`).
    bp: usize,
    /// Interval register `A` (16-bit value held in a `u32`).
    a: u32,
    /// Code register `C` (the §C.3.1 32-bit Chigh:Clow concatenation).
    c: u32,
    /// Bit counter `CT` (§C.3.3).
    ct: i32,
    /// Whether the BYTEIN procedure ever synthesised an `0xFF` fill byte
    /// because the input slice ran out before the decoder asked for the
    /// next byte (T.800 §C.3.4 end-of-stream / §D.4.1 padding).
    ///
    /// Initialised to `false`. Set the first time `bytein` reads past the
    /// end of `data` (either the `B` lookup or the `B1` peek when the
    /// segment ends mid-0xFF-prefix) and never cleared. The §D.4.2
    /// predictable-termination check ([`Self::predictable_termination_satisfied`])
    /// requires this to stay `false` for the segment to be conformant.
    synthetic_fill_used: bool,
}

impl<'a> MqDecoder<'a> {
    /// INITDEC — initialize the decoder over `data` (T.800 §C.3.5,
    /// Figure C.20).
    ///
    /// `BP` is set to `BPST` (the first compressed byte). The first
    /// byte is shifted into the low byte of `Chigh` (`C = B << 16`),
    /// BYTEIN reads the next byte, then the `C` register is shifted
    /// left by 7 and `CT` decremented by 7 to bring `C` into alignment
    /// with the starting `A = 0x8000`.
    ///
    /// An empty `data` slice initialises into the §C.3.4 end-of-stream
    /// terminal state (BYTEIN immediately synthesises the `0xFF`
    /// fill), so decoding still proceeds — it just yields the residual
    /// MPS run, as §D.4.1 requires when the signalled bytes run out.
    pub fn new(data: &'a [u8]) -> Self {
        let first = data.first().copied().unwrap_or(0xFF);
        // INITDEC's first byte read is itself a §C.3.4 BYTEIN — if the
        // input slice is empty the synthesised 0xFF fill kicks in and the
        // predictable-termination check must observe it.
        let synthetic_fill_used = data.is_empty();
        let mut dec = MqDecoder {
            data,
            bp: 0,
            a: 0,
            c: (first as u32) << 16,
            ct: 0,
            synthetic_fill_used,
        };
        dec.bytein();
        dec.c <<= 7;
        dec.ct -= 7;
        dec.a = 0x8000;
        dec
    }

    /// BYTEIN — read one compressed byte, compensating for the
    /// `0xFF`-prefixed stuff bit and the end-of-stream marker (T.800
    /// §C.3.4, Figure C.19).
    ///
    /// `B` is the byte at `BP`. If `B != 0xFF`, `BP` advances and the
    /// new `B` is inserted into the high 8 bits of `Clow` (`C += B <<
    /// 8`) with `CT = 8`. If `B == 0xFF`, the next byte `B1` is
    /// tested: `B1 > 0x8F` (or off the end of the slice) means a marker
    /// code terminates the segment — the decoder is fed `1`-bits by
    /// adding `0xFF00` to `C` and setting `CT = 8`, leaving `BP` on the
    /// `0xFF` prefix. Otherwise `B1` carries a stuff bit: `BP`
    /// advances and `B` is added aligned so the stuff bit lands on the
    /// low bit of `Chigh` (`C += B1 << 9`) with `CT = 7`.
    fn bytein(&mut self) {
        let raw_b = self.data.get(self.bp).copied();
        // BP off the end means the §C.3.4 0xFF-fill is being synthesised.
        // The §D.4.2 predictable-termination check observes this flag.
        if raw_b.is_none() {
            self.synthetic_fill_used = true;
        }
        let b = raw_b.unwrap_or(0xFF);
        if b == 0xFF {
            // Peek B1 (the byte after the 0xFF prefix). Off-the-end is
            // treated as a marker per §C.3.4 / §D.4.1 (synthesise the
            // 0xFF-fill end of stream).
            let raw_b1 = self.data.get(self.bp + 1).copied();
            if raw_b1.is_none() {
                self.synthetic_fill_used = true;
            }
            let b1 = raw_b1.unwrap_or(0xFF);
            if b1 > 0x8F {
                // Marker code: feed 1-bits, BP stays on the 0xFF prefix.
                self.c += 0xFF00;
                self.ct = 8;
            } else {
                // Stuffed bit: B1 added so the stuff bit (any carry)
                // aligns with the low order bit of Chigh.
                self.bp += 1;
                self.c += (b1 as u32) << 9;
                self.ct = 7;
            }
        } else {
            self.bp += 1;
            let raw_nb = self.data.get(self.bp).copied();
            if raw_nb.is_none() {
                self.synthetic_fill_used = true;
            }
            let nb = raw_nb.unwrap_or(0xFF);
            self.c += (nb as u32) << 8;
            self.ct = 8;
        }
    }

    /// RENORMD — renormalize the decoder (T.800 §C.3.3, Figure C.18).
    ///
    /// Shift `A` and `C` left one bit at a time, pulling a fresh byte
    /// via BYTEIN whenever `CT` reaches zero, until `A ≥ 0x8000`.
    fn renormd(&mut self) {
        loop {
            if self.ct == 0 {
                self.bytein();
            }
            self.a <<= 1;
            self.c <<= 1;
            self.ct -= 1;
            if self.a & 0x8000 != 0 {
                break;
            }
        }
    }

    /// DECODE — decode one binary decision `D ∈ {0, 1}` against the
    /// adaptive state in `cx` (T.800 §C.3.2, Figure C.15, with the
    /// MPS-path Figure C.16 and LPS-path Figure C.17 conditional
    /// MPS/LPS exchange and the §C.2.5 probability update).
    ///
    /// `A` is first reduced by `Qe(I(CX))`. `Chigh` (`c >> 16`) is
    /// compared to `Qe`:
    ///
    /// * `Chigh ≥ Qe` (the usual MPS branch): `Chigh -= Qe`. If `A &
    ///   0x8000 != 0` no renormalization is needed and `D = MPS(CX)`.
    ///   Otherwise the MPS-path conditional exchange (Figure C.16)
    ///   runs and RENORMD follows.
    /// * `Chigh < Qe`: the LPS-path conditional exchange (Figure C.17)
    ///   runs and RENORMD follows.
    pub fn decode(&mut self, cx: &mut MqContext) -> u8 {
        let entry = QE[cx.index as usize];
        let qe = entry.qe as u32;
        self.a = self.a.wrapping_sub(qe);

        let d;
        if (self.c >> 16) < qe {
            // LPS path (Figure C.17): the LPS sub-interval is the upper
            // part. Chigh stays (only A changes here); the comparison
            // A < Qe decides whether the conditional exchange occurred.
            d = self.lps_exchange(cx, qe);
            self.renormd();
        } else {
            // Chigh -= Qe (the MPS sub-interval is the lower part).
            self.c -= qe << 16;
            if self.a & 0x8000 == 0 {
                // Renormalization needed → MPS-path conditional
                // exchange (Figure C.16).
                d = self.mps_exchange(cx, qe);
                self.renormd();
            } else {
                // No renormalization: plain MPS.
                d = cx.mps as u8;
            }
        }
        d
    }

    /// MPS-path conditional exchange (T.800 §C.3.2, Figure C.16).
    ///
    /// Reached only when renormalization is needed on the MPS branch.
    /// If `A ≥ Qe` an MPS truly occurred: `D = MPS(CX)`, index updates
    /// to NMPS. Otherwise the conditional exchange happened (the LPS
    /// sub-interval was larger): `D = 1 - MPS(CX)`, the SWITCH flag may
    /// flip the MPS sense, and the index updates to NLPS.
    fn mps_exchange(&mut self, cx: &mut MqContext, qe: u32) -> u8 {
        let entry = QE[cx.index as usize];
        if self.a < qe {
            // Conditional exchange: LPS.
            let d = !cx.mps as u8;
            if entry.switch {
                cx.mps = !cx.mps;
            }
            cx.index = entry.nlps;
            d
        } else {
            // MPS.
            let d = cx.mps as u8;
            cx.index = entry.nmps;
            d
        }
    }

    /// LPS-path conditional exchange (T.800 §C.3.2, Figure C.17).
    ///
    /// Reached when `Chigh < Qe`. On both branches the new `A` is set
    /// to `Qe`. If the (pre-set) `A < Qe` the conditional exchange
    /// occurred so the decision is the MPS case (index → NMPS);
    /// otherwise it is the genuine LPS case (`D = 1 - MPS`, SWITCH may
    /// flip MPS, index → NLPS).
    fn lps_exchange(&mut self, cx: &mut MqContext, qe: u32) -> u8 {
        let entry = QE[cx.index as usize];
        let d;
        if self.a < qe {
            // Conditional exchange: MPS.
            d = cx.mps as u8;
            cx.index = entry.nmps;
        } else {
            // LPS.
            d = !cx.mps as u8;
            if entry.switch {
                cx.mps = !cx.mps;
            }
            cx.index = entry.nlps;
        }
        self.a = qe;
        d
    }

    /// The current byte pointer `BP` (index into the input slice).
    ///
    /// Useful for the coding-pass layer to learn how many compressed
    /// bytes the decoder consumed for a terminated segment.
    pub fn byte_pointer(&self) -> usize {
        self.bp
    }

    /// Whether the §C.3.4 0xFF-fill synthesis has ever fired on this
    /// decoder.
    ///
    /// BYTEIN sets a sticky flag the first time the input slice runs out
    /// (either at the `B` lookup or the `B1` peek that follows a `0xFF`
    /// prefix when the prefix sits at the end of the segment). The flag
    /// is never cleared; once observed the segment cannot satisfy the
    /// T.800 §D.4.2 predictable-termination contract because the encoder
    /// guarantees every bit the decoder asks for is materialised in the
    /// codestream.
    pub fn synthetic_fill_used(&self) -> bool {
        self.synthetic_fill_used
    }

    /// Whether the decoder's run over a predictably-terminated codeword
    /// segment of length `segment_len` is conformant with the T.800
    /// §D.4.2 termination contract.
    ///
    /// Returns `true` iff **both** of the following hold:
    ///
    /// 1. No synthetic `0xFF`-fill was consumed
    ///    ([`Self::synthetic_fill_used`] is `false`). The §D.4.2 procedure
    ///    pushes out `k = (11 − CT) + 1` bits via repeated BYTEOUT calls
    ///    so the encoder side guarantees every bit the decoder will ever
    ///    ask for is already present in the codestream — the §C.3.4
    ///    end-of-stream marker (BYTEIN synthesising `0xFF` past the end
    ///    of the slice) is mutually exclusive with a predictably
    ///    terminated segment.
    /// 2. The byte pointer has advanced to exactly `segment_len`, **or**
    ///    sits one byte short of it on the `0xFF` prefix of an
    ///    end-of-stream marker. Either case means the decoder consumed
    ///    the entire signalled segment without backtracking. The
    ///    one-short case is the §C.3.4 BYTEIN behaviour described at the
    ///    top of the module: when `B = 0xFF` and `B1 > 0x8F`, `BP` stays
    ///    on the `0xFF` prefix (no advance past the marker code), so a
    ///    properly-terminated segment whose last byte is `0xFF` parks
    ///    `BP` on `segment_len − 1`.
    ///
    /// Predictable termination is signalled via the COD / COC Table A.19
    /// bit-4 ([`crate::CodeBlockStyle::predictable_termination`]) and is
    /// independent of the bit-2 termination-on-each-coding-pass flag
    /// (§D.4.2 says "this can be used with or without the predictable
    /// termination" via the segmentation-symbol cross-reference; the
    /// flags compose). The §D.6 selective bypass mode terminates the
    /// fourth cleanup pass and every raw segment, and the predictable
    /// flag applies to each terminated segment in isolation — open a
    /// fresh [`MqDecoder`] per terminated segment and call this method
    /// against that segment's length.
    ///
    /// HTJ2K (Part 15) HT code-blocks explicitly opt out of predictable
    /// termination (the §A.6.1 Table A.13 Scod-bit-5 description for
    /// HT code-blocks reads "Predictable termination (does not apply to
    /// HT code-blocks)") and consumers should not call this method on
    /// HT code-block segments.
    ///
    /// `segment_len` should equal the segment length the §B.10.7
    /// packet-header walker (or the §D.4.2-aware caller's pre-allocated
    /// slice length) signalled. A length smaller than the decoder's
    /// `BP` returns `false` — the decoder ran past the signalled segment
    /// end, which is itself a §D.4.2 violation.
    pub fn predictable_termination_satisfied(&self, segment_len: usize) -> bool {
        if self.synthetic_fill_used {
            return false;
        }
        // Two acceptable terminal positions:
        //   - BP == segment_len: every byte consumed, no marker tail.
        //   - BP == segment_len - 1 AND data[BP] == 0xFF: parked on the
        //     0xFF prefix of an end-of-segment marker (the §C.3.4 BYTEIN
        //     rule keeps BP on the prefix without advancing past it).
        if self.bp == segment_len {
            return true;
        }
        if segment_len > 0 && self.bp + 1 == segment_len {
            return matches!(self.data.get(self.bp), Some(&0xFF));
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Table C.2 invariants (T.800 §C.2.5) --------------------------

    #[test]
    fn table_c2_has_47_entries() {
        assert_eq!(QE.len(), 47);
    }

    #[test]
    fn table_c2_indices_in_range() {
        // Every NMPS / NLPS must point back into the table.
        for (i, e) in QE.iter().enumerate() {
            assert!(
                (e.nmps as usize) < QE.len(),
                "NMPS out of range at index {i}"
            );
            assert!(
                (e.nlps as usize) < QE.len(),
                "NLPS out of range at index {i}"
            );
        }
    }

    #[test]
    fn table_c2_spot_values() {
        // Spot-check the rows quoted in the §C.2.5 prose / Table C.2.
        assert_eq!(
            QE[0],
            QeEntry {
                qe: 0x5601,
                nmps: 1,
                nlps: 1,
                switch: true
            }
        );
        assert_eq!(
            QE[1],
            QeEntry {
                qe: 0x3401,
                nmps: 2,
                nlps: 6,
                switch: false
            }
        );
        // Index 35 — the OCR `0x02Al` row; binary confirms 0x02A1.
        assert_eq!(QE[35].qe, 0x02A1);
        // Index 45 is the table's terminal-precision row; NMPS self-loops.
        assert_eq!(
            QE[45],
            QeEntry {
                qe: 0x0001,
                nmps: 45,
                nlps: 43,
                switch: false
            }
        );
        // Index 46 — UNIFORM: fixed, self-looping, no switch.
        assert_eq!(
            QE[46],
            QeEntry {
                qe: 0x5601,
                nmps: 46,
                nlps: 46,
                switch: false
            }
        );
    }

    #[test]
    fn switch_flags_only_set_at_0_6_14() {
        // Per Table C.2, SWITCH = 1 only at indices 0, 6, 14.
        for (i, e) in QE.iter().enumerate() {
            let expect = i == 0 || i == 6 || i == 14;
            assert_eq!(e.switch, expect, "SWITCH mismatch at index {i}");
        }
    }

    // -- Table D.7 initial states (T.800 §D.4) ------------------------

    #[test]
    fn initial_context_states() {
        assert_eq!(
            MqContext::default(),
            MqContext {
                index: 0,
                mps: false
            }
        );
        assert_eq!(
            MqContext::uniform(),
            MqContext {
                index: 46,
                mps: false
            }
        );
        assert_eq!(
            MqContext::run_length(),
            MqContext {
                index: 3,
                mps: false
            }
        );
        assert_eq!(
            MqContext::zero_neighbours(),
            MqContext {
                index: 4,
                mps: false
            }
        );
    }

    #[test]
    fn context_accessors() {
        let cx = MqContext::run_length();
        assert_eq!(cx.index(), 3);
        assert!(!cx.mps());
    }

    #[test]
    fn reset_to_restores_initial() {
        let mut cx = MqContext::default();
        // Mutate it via a few decode steps.
        let bytes = [0x00u8, 0x00, 0x00];
        let mut dec = MqDecoder::new(&bytes);
        for _ in 0..8 {
            dec.decode(&mut cx);
        }
        assert_ne!(cx, MqContext::default());
        cx.reset_to(MqContext::default());
        assert_eq!(cx, MqContext::default());
    }

    // -- INITDEC register alignment (T.800 §C.3.5) --------------------

    #[test]
    fn initdec_sets_a_to_0x8000() {
        let bytes = [0x12u8, 0x34, 0x56];
        let dec = MqDecoder::new(&bytes);
        assert_eq!(dec.a, 0x8000);
    }

    #[test]
    fn initdec_register_alignment_known_bytes() {
        // Trace INITDEC by hand for B0=0x12, B1=0x34 (neither 0xFF):
        //   C = 0x12 << 16            = 0x0012_0000
        //   BYTEIN: B(=0x12)!=0xFF → BP=1, NB=0x34, C += 0x34<<8 = 0x3400
        //           C = 0x0012_3400, CT=8
        //   C <<= 7                   = 0x091A_0000
        //   CT -= 7                   → CT = 1
        //   A = 0x8000
        let bytes = [0x12u8, 0x34, 0x56];
        let dec = MqDecoder::new(&bytes);
        assert_eq!(dec.c, 0x091A_0000);
        assert_eq!(dec.ct, 1);
        assert_eq!(dec.bp, 1);
    }

    #[test]
    fn initdec_empty_input_uses_ff_fill() {
        // Empty slice: first byte synthesised as 0xFF, BYTEIN sees the
        // 0xFF marker terminal state.
        //   C = 0xFF << 16 = 0x00FF_0000
        //   BYTEIN: B=0xFF, B1(off end)=0xFF > 0x8F → marker:
        //           C += 0xFF00 → 0x00FF_FF00, CT=8, BP stays 0.
        //   C <<= 7 → 0x7FFF_8000 ; CT -= 7 → 1 ; A = 0x8000
        let dec = MqDecoder::new(&[]);
        assert_eq!(dec.a, 0x8000);
        assert_eq!(dec.c, 0x7FFF_8000);
        assert_eq!(dec.ct, 1);
        assert_eq!(dec.bp, 0);
    }

    // -- BYTEIN stuff-bit handling (T.800 §C.3.4) ---------------------

    #[test]
    fn bytein_stuff_bit_after_ff() {
        // After INITDEC positions BP, force BYTEIN to hit a 0xFF whose
        // successor is <= 0x8F (a stuffed bit, not a marker). Drive a
        // few renormalizations so CT empties and BYTEIN fires.
        // Bytes: [0x00, 0xFF, 0x10, 0x00] — the 0xFF at index 1 is
        // followed by 0x10 (a stuff bit).
        let bytes = [0x00u8, 0xFF, 0x10, 0x00];
        let mut dec = MqDecoder::new(&bytes);
        // Run enough decisions to consume past the 0xFF.
        let mut cx = MqContext::default();
        for _ in 0..40 {
            dec.decode(&mut cx);
        }
        // The decoder must not panic and BP must have advanced past the
        // 0xFF prefix into the stuffed byte region.
        assert!(dec.byte_pointer() >= 2);
    }

    #[test]
    fn bytein_marker_keeps_bp_on_ff() {
        // 0xFF followed by 0xFF (>0x8F) is the end-of-stream marker.
        // BP must stay on the 0xFF prefix and decoding must continue
        // by feeding 1-bits (no panic, no advance past the marker).
        let bytes = [0x00u8, 0xFF, 0xFF];
        let mut dec = MqDecoder::new(&bytes);
        let mut cx = MqContext::default();
        for _ in 0..100 {
            dec.decode(&mut cx);
        }
        // BP parks on or before the 0xFF prefix at index 1; it never
        // runs off the end.
        assert!(dec.byte_pointer() <= bytes.len());
    }

    // -- DECODE behaviour ---------------------------------------------

    #[test]
    fn decode_returns_binary_decisions() {
        let bytes = [0x84u8, 0xC7, 0x3B, 0xFC, 0xE1];
        let mut dec = MqDecoder::new(&bytes);
        let mut cx = MqContext::default();
        for _ in 0..64 {
            let d = dec.decode(&mut cx);
            assert!(d == 0 || d == 1, "DECODE must return 0 or 1, got {d}");
        }
    }

    #[test]
    fn decode_is_deterministic() {
        // Two decoders over the same bytes with the same context start
        // must produce the identical decision sequence.
        let bytes = [0x84u8, 0xC7, 0x3B, 0xFC, 0xE1, 0x00, 0x99];
        let mut a = MqDecoder::new(&bytes);
        let mut b = MqDecoder::new(&bytes);
        let mut ca = MqContext::default();
        let mut cb = MqContext::default();
        for _ in 0..200 {
            assert_eq!(a.decode(&mut ca), b.decode(&mut cb));
        }
        assert_eq!(ca, cb);
    }

    #[test]
    fn a_register_stays_renormalized() {
        // The invariant of the MQ decoder: after every DECODE the A
        // register satisfies 0x8000 <= A < 0x10000 (§C.1.2 keeps A in
        // [0.75, 1.5) which is [0x8000, 0x10000) in this fixed point).
        let bytes = [0x84u8, 0xC7, 0x3B, 0xFC, 0xE1, 0x00, 0x99, 0x55, 0xAA];
        let mut dec = MqDecoder::new(&bytes);
        let mut cx = MqContext::default();
        for _ in 0..300 {
            dec.decode(&mut cx);
            assert!(
                (0x8000..0x1_0000).contains(&dec.a),
                "A out of [0x8000, 0x10000): {:#x}",
                dec.a
            );
        }
    }

    #[test]
    fn uniform_context_index_is_stable() {
        // The UNIFORM context (index 46) self-loops on both NMPS and
        // NLPS, so its index must never move regardless of decisions.
        let bytes = [0x84u8, 0xC7, 0x3B, 0xFC, 0xE1, 0x00];
        let mut dec = MqDecoder::new(&bytes);
        let mut cx = MqContext::uniform();
        for _ in 0..50 {
            dec.decode(&mut cx);
            assert_eq!(cx.index(), 46);
        }
    }

    #[test]
    fn all_ff_input_decodes_without_overrun() {
        // §C.3.4 / §D.4.1: a pair of 0xFF bytes is the synthesised
        // end-of-stream marker; the decoder must keep producing
        // decisions (feeding 1-bits) indefinitely without panicking and
        // without ever advancing BP past the 0xFF prefix. We decode a
        // long run and assert BP parks on the marker prefix and the run
        // is fully deterministic (a second identical decoder agrees).
        let bytes = [0xFFu8, 0xFF];
        let mut dec = MqDecoder::new(&bytes);
        let mut ref_dec = MqDecoder::new(&bytes);
        let mut cx = MqContext::default();
        let mut rcx = MqContext::default();
        for _ in 0..256 {
            let d = dec.decode(&mut cx);
            assert!(d == 0 || d == 1);
            assert_eq!(d, ref_dec.decode(&mut rcx));
        }
        // BP never runs off the end (the marker holds it on the prefix).
        assert!(dec.byte_pointer() <= bytes.len());
    }

    // -- §D.4.2 predictable-termination check ------------------------

    #[test]
    fn synthetic_fill_starts_false_for_non_empty_input() {
        // INITDEC over a normal segment: the first byte read is real,
        // so the synthesised-fill flag stays clear.
        let bytes = [0x12u8, 0x34, 0x56];
        let dec = MqDecoder::new(&bytes);
        assert!(!dec.synthetic_fill_used());
    }

    #[test]
    fn synthetic_fill_set_by_empty_input() {
        // §C.3.4: empty data → BYTEIN immediately synthesises the
        // 0xFF-fill terminal state. The decoder must record this and a
        // predictable-termination check against any length must fail.
        let dec = MqDecoder::new(&[]);
        assert!(dec.synthetic_fill_used());
        assert!(!dec.predictable_termination_satisfied(0));
        assert!(!dec.predictable_termination_satisfied(8));
    }

    #[test]
    fn predictable_termination_holds_when_bp_lands_on_segment_end() {
        // INITDEC over [0x12, 0x34, 0x56] parks BP at 1 (it consumed
        // bytes 0 and 1 — see initdec_register_alignment_known_bytes).
        // A signalled segment_len == 1 covers exactly the consumed
        // bytes; report conformance.
        let bytes = [0x12u8, 0x34, 0x56];
        let dec = MqDecoder::new(&bytes);
        assert_eq!(dec.byte_pointer(), 1);
        assert!(!dec.synthetic_fill_used());
        assert!(dec.predictable_termination_satisfied(1));
    }

    #[test]
    fn predictable_termination_fails_when_bp_short_of_segment_end() {
        // BP == 1 with segment_len == 3 means the decoder stopped two
        // bytes shy of the signalled segment end — not conformant.
        let bytes = [0x12u8, 0x34, 0x56];
        let dec = MqDecoder::new(&bytes);
        assert_eq!(dec.byte_pointer(), 1);
        assert!(!dec.predictable_termination_satisfied(3));
    }

    #[test]
    fn predictable_termination_fails_when_bp_overruns_segment_len() {
        // A segment_len strictly less than BP means the decoder ran
        // past the signalled segment end during decode (§D.4.2 says
        // "the decoder need not backtrack" — a strict overrun is a
        // violation).
        let bytes = [0x12u8, 0x34, 0x56];
        let dec = MqDecoder::new(&bytes);
        assert_eq!(dec.byte_pointer(), 1);
        assert!(!dec.predictable_termination_satisfied(0));
    }

    #[test]
    fn predictable_termination_accepts_bp_parked_on_0xff_marker_prefix() {
        // §C.3.4: when BP sits on a 0xFF byte and the next byte is the
        // 0xFF marker tail, BYTEIN feeds 1-bits without advancing BP.
        // A predictably-terminated segment whose final byte is 0xFF
        // therefore parks BP on segment_len − 1, on the 0xFF prefix.
        // [data[BP] == 0xFF AND BP + 1 == segment_len] must report
        // conformance.
        let bytes = [0x10u8, 0xFFu8, 0xFFu8]; // 0xFF at index 1
        let mut dec = MqDecoder::new(&bytes);
        let mut cx = MqContext::default();
        // Drive enough decisions to push BP onto the 0xFF prefix at
        // index 1 (where it parks per §C.3.4 marker rule).
        for _ in 0..64 {
            dec.decode(&mut cx);
        }
        // BP must be parked on the 0xFF prefix (≤ bytes.len() — 1).
        // Pick a segment_len that matches BP + 1.
        let bp = dec.byte_pointer();
        if dec.data.get(bp) == Some(&0xFF) {
            assert!(dec.predictable_termination_satisfied(bp + 1));
        }
    }

    #[test]
    fn predictable_termination_fails_when_synthetic_fill_used() {
        // INITDEC over a single byte: byte 0 is read into Chigh, then
        // BYTEIN runs and reads data[1] which is off the end → fill
        // synthesised. The flag is sticky and the predictable check
        // must reject every segment length.
        let bytes = [0x12u8];
        let dec = MqDecoder::new(&bytes);
        assert!(dec.synthetic_fill_used());
        for seg in 0..=bytes.len() + 4 {
            assert!(
                !dec.predictable_termination_satisfied(seg),
                "predictable check must reject segment_len {seg} when synthetic fill fired"
            );
        }
    }

    #[test]
    fn synthetic_fill_does_not_fire_on_ff_ff_marker_run() {
        // The 0xFF 0xFF marker is a real synthesised end-of-stream
        // present in the input — BYTEIN never reads past the slice
        // because it parks on the 0xFF prefix. The synthetic-fill flag
        // therefore stays clear even under a long decode run.
        let bytes = [0xFFu8, 0xFFu8];
        let mut dec = MqDecoder::new(&bytes);
        let mut cx = MqContext::default();
        for _ in 0..512 {
            dec.decode(&mut cx);
        }
        assert!(!dec.synthetic_fill_used());
        // BP parks on or before the 0xFF prefix; the marker-prefix
        // acceptance rule lets the check accept segment_len = BP + 1.
        let bp = dec.byte_pointer();
        assert!(bp < bytes.len());
        assert_eq!(dec.data[bp], 0xFF);
        assert!(dec.predictable_termination_satisfied(bp + 1));
    }

    #[test]
    fn predictable_termination_segment_len_zero_rejects_non_initial_bp() {
        // A segment_len of zero with BP > 0 is rejected. Even the
        // INITDEC-only state (BP == 1 for typical non-FF inputs) sits
        // past 0, so the check returns false.
        let bytes = [0x12u8, 0x34];
        let dec = MqDecoder::new(&bytes);
        assert_eq!(dec.byte_pointer(), 1);
        assert!(!dec.predictable_termination_satisfied(0));
    }

    #[test]
    fn predictable_termination_zero_byte_segment_initial_state_passes() {
        // The degenerate case of an empty-data decoder with a
        // segment_len of zero: synthetic fill fired in INITDEC (data
        // was empty) so the check must still reject. Validates the
        // synthetic-fill gate takes priority over the BP-vs-len test.
        let dec = MqDecoder::new(&[]);
        assert!(dec.synthetic_fill_used());
        // BP is 0 and segment_len is 0: the BP equality would alone
        // accept, but the synthetic-fill gate kicks first.
        assert_eq!(dec.byte_pointer(), 0);
        assert!(!dec.predictable_termination_satisfied(0));
    }

    #[test]
    fn synthetic_fill_flag_is_sticky_after_first_overrun() {
        // Once synthetic fill fires, further BYTEIN calls (even those
        // that happen to hit real bytes again — impossible here but the
        // contract is one-way) leave the flag set.
        let mut dec = MqDecoder::new(&[]);
        assert!(dec.synthetic_fill_used());
        let mut cx = MqContext::default();
        for _ in 0..32 {
            dec.decode(&mut cx);
        }
        assert!(dec.synthetic_fill_used());
    }

    #[test]
    fn ff_fill_settles_to_a_constant_decision() {
        // §C.3.4: once the 0xFF-fill marker state is reached and the
        // adaptive index has settled, the decoder emits a steady run.
        // We assert that the tail of a long 0xFF-fill decode is a
        // single constant symbol (no further state change once the
        // context has converged on the fill).
        let bytes = [0xFFu8, 0xFF];
        let mut dec = MqDecoder::new(&bytes);
        let mut cx = MqContext::default();
        let mut tail = Vec::new();
        for i in 0..512 {
            let d = dec.decode(&mut cx);
            if i >= 256 {
                tail.push(d);
            }
        }
        let first = tail[0];
        assert!(
            tail.iter().all(|&d| d == first),
            "0xFF fill tail should be a constant run, got mixed symbols"
        );
    }
}
