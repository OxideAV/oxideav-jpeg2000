//! EBCOT tier-1 bit-plane encoder (ISO/IEC 15444-1 Annex D).
//!
//! Mirror of [`crate::decode::t1`]. Walks a signed-magnitude code-block
//! bit-plane by bit-plane and emits the three coding passes
//! (significance propagation, magnitude refinement, cleanup) through
//! the MQ arithmetic encoder. The output byte stream is what tier-2
//! packs into packets.
//!
//! Only the minimum feature set for baseline lossless encode is
//! implemented:
//!
//! - All three passes are arithmetic-coded (no SPB bypass, no RESTART,
//!   no VSC, no SEGSYM, no PTERM).
//! - The block is treated as a single segment terminated by a single
//!   `flush` at the end.
//!
//! Input is an `i32` plane of sign-magnitude samples in the same
//! representation the decoder produces (after `oneplushalf` / magref
//! decoding): the magnitude is the absolute value and the sign is
//! carried by the sign bit. The encoder handles the sign conversion
//! internally.

use super::mqc::MqcEnc;
use crate::decode::mqc::{CTX_AGG, CTX_MAG, CTX_UNI, CTX_ZC};
use crate::decode::t1::trace as dec_trace;
use crate::decode::t1::Orient;

/// Number of MSB zero bit-planes for every sample in the code-block —
/// encoder-side equivalent of the decoder's `missing_msb`. This drives
/// the starting bit-plane for the three EBCOT passes.
pub struct EncodedCblk {
    /// Compressed byte stream for this code-block.
    pub data: Vec<u8>,
    /// Total number of bit-plane passes carried in this block.
    pub total_passes: u32,
    /// Number of MSB bit-planes that are entirely zero. Feeds directly
    /// into the zero-bitplane tag-tree at tier-2.
    pub missing_msb: u32,
}

/// Encode one code-block.
///
/// - `samples`: flat `w * h` array of signed sample values. Positive
///   magnitudes are positive, negatives are negative (the usual i32).
/// - `band_numbps`: `guard_bits + epsilon_b - 1` for this band (T.800
///   Eq E-2). Determines the maximum bit plane we code.
/// - `orient`: sub-band orientation (for ZC context lookup).
pub fn encode_cblk(
    samples: &[i32],
    w: usize,
    h: usize,
    band_numbps: i32,
    orient: Orient,
) -> EncodedCblk {
    debug_assert_eq!(samples.len(), w * h);

    // Build magnitude and sign arrays.
    let mut mag = vec![0u32; w * h];
    let mut sign = vec![false; w * h];
    let mut max_mag = 0u32;
    for i in 0..w * h {
        let v = samples[i];
        // Tier-1 output magnitudes are already multiplied by 2 (the
        // `oneplushalf` convention) when coming from the decoder —
        // encoder *input* follows the same convention: the magnitude
        // is absolute value, which for coefficients emitted by the
        // forward 5/3 transform is already in the natural range.
        // Multiply by 2 to match the decoder's representation so
        // sigprop will deposit `oneplushalf = 1.5 * 2^bpno` at the
        // correct bit-plane.
        let m = v.unsigned_abs();
        if m > max_mag {
            max_mag = m;
        }
        mag[i] = m;
        sign[i] = v < 0;
    }

    // Scale magnitudes into the tier-1 "oneplushalf" representation —
    // shift left by 1 so the MSB bit aligns with bpno + 1. This lets
    // us use the same bit-plane numbering as the decoder: bit `bpno`
    // of the magnitude corresponds to plane `bpno`.
    //
    // Equivalently: `mag_t1 = mag * 2`.
    for m in &mut mag {
        *m <<= 1;
    }
    // Recompute max.
    let max_t1 = max_mag << 1;

    // Decoder's tag-tree threshold sweep returns `i = leaf + 1`, which
    // is consumed as `cblk.missing_msb`. The decoder's starting bit
    // plane is `bpno = band_numbps + 1 - i = band_numbps - leaf`. So
    // for the encoder we pick `leaf` (what we write into the tag tree,
    // stored as `missing_msb` here for symmetry) = `band_numbps -
    // bpno_start`, where `bpno_start` = MSB position of the largest
    // pre-shift magnitude (zero-based). For `max_t1 = 0` we send
    // `leaf = band_numbps + 1` — there are no populated bit planes to
    // encode.
    //
    // `band_numbps` above is T.800's `Mb - 1`, so Mb = band_numbps + 1.
    let mb = band_numbps + 1;
    let (leaf, bpno_start): (u32, i32) = if max_t1 == 0 {
        (mb as u32, 0)
    } else {
        let msb_pos = 31 - max_t1.leading_zeros() as i32;
        let clamped_msb = msb_pos.min(band_numbps);
        let leaf = (band_numbps - clamped_msb) as u32;
        (leaf, clamped_msb)
    };
    let missing_msb = leaf;
    let mut bpno = bpno_start;
    if bpno < 0 {
        bpno = 0;
    }

    // Encoder state mirrors the decoder's `DecoderState`.
    let mut state = EncoderState::new(w, h, mag, sign);
    let mut mqc = MqcEnc::new();
    mqc.reset_states();

    let mut total_passes: u32 = 0;
    // First pass at the highest bit plane is a cleanup (per T.800 §D.2).
    // We stop at `cur_bpno >= 1` to stay clear of the `bpno - 1` shift
    // in magref that would otherwise require a signed-0 shift; our
    // decoder assumes the same bound. Bit-exact 5/3 lossless stays
    // reachable because the encoder left-shifts the magnitude by one
    // bit at input (see above), which makes the last-coded bit plane
    // the natural bit-0 of the pre-shift magnitude.
    let mut passtype: u32 = 2;
    let mut cur_bpno = bpno;
    while cur_bpno >= 1 {
        match passtype {
            0 => sigprop_pass_enc(&mut state, &mut mqc, cur_bpno, orient),
            1 => magref_pass_enc(&mut state, &mut mqc, cur_bpno),
            2 => cleanup_pass_enc(&mut state, &mut mqc, cur_bpno, orient),
            _ => {}
        }
        total_passes += 1;
        passtype += 1;
        if passtype == 3 {
            passtype = 0;
            cur_bpno -= 1;
        }
    }
    mqc.flush();
    let data = mqc.finish();
    EncodedCblk {
        data,
        total_passes,
        missing_msb,
    }
}

/// Encoder scratch state — magnitudes + signs (inputs) plus per-sample
/// sigma / mu / pi flags that track encoding progress. Mirrors the
/// decoder's `DecoderState` so that every MQ decision we make here
/// also yields the same state transition the decoder will observe.
struct EncoderState {
    w: usize,
    h: usize,
    mag: Vec<u32>,
    sign: Vec<bool>,
    sigma: Vec<bool>,
    /// True after the first magnitude refinement pass for this sample.
    mu: Vec<bool>,
    /// True if the sample was coded in sigprop *this* bit plane.
    pi: Vec<bool>,
}

impl EncoderState {
    fn new(w: usize, h: usize, mag: Vec<u32>, sign: Vec<bool>) -> Self {
        let n = w * h;
        EncoderState {
            w,
            h,
            mag,
            sign,
            sigma: vec![false; n],
            mu: vec![false; n],
            pi: vec![false; n],
        }
    }

    #[inline]
    fn idx(&self, x: usize, y: usize) -> usize {
        y * self.w + x
    }

    /// Is bit `bpno` of the (pre-scaled) magnitude set?
    #[inline]
    fn bit_at(&self, x: usize, y: usize, bpno: i32) -> u32 {
        if bpno < 0 {
            return 0;
        }
        (self.mag[self.idx(x, y)] >> bpno) & 1
    }

    /// 8-neighbour significance count helper. Returns (h, v, d): count
    /// of significant horizontal (E/W), vertical (N/S), and diagonal
    /// neighbours. Mirrors `DecoderState::hvd_counts`.
    fn hvd_counts(&self, x: usize, y: usize) -> (u32, u32, u32) {
        let w = self.w as isize;
        let h = self.h as isize;
        let mut m = 0u8;
        let bits: [(isize, isize, u8); 8] = [
            (-1, -1, 1 << 0), // NW
            (0, -1, 1 << 1),  // N
            (1, -1, 1 << 2),  // NE
            (-1, 0, 1 << 3),  // W
            (1, 0, 1 << 4),   // E
            (-1, 1, 1 << 5),  // SW
            (0, 1, 1 << 6),   // S
            (1, 1, 1 << 7),   // SE
        ];
        for (dx, dy, b) in bits {
            let nx = x as isize + dx;
            let ny = y as isize + dy;
            if nx >= 0
                && nx < w
                && ny >= 0
                && ny < h
                && self.sigma[self.idx(nx as usize, ny as usize)]
            {
                m |= b;
            }
        }
        // Horizontals: W (bit 3), E (bit 4)
        // Verticals: N (bit 1), S (bit 6)
        // Diagonals: NW (bit 0), NE (bit 2), SW (bit 5), SE (bit 7)
        let hh = ((m >> 3) & 1) as u32 + ((m >> 4) & 1) as u32;
        let vv = ((m >> 1) & 1) as u32 + ((m >> 6) & 1) as u32;
        let dd =
            (m & 1) as u32 + ((m >> 2) & 1) as u32 + ((m >> 5) & 1) as u32 + ((m >> 7) & 1) as u32;
        (hh, vv, dd)
    }

    /// Is any 8-neighbour significant?
    fn any_neighbour_sig(&self, x: usize, y: usize) -> bool {
        let (h, v, d) = self.hvd_counts(x, y);
        (h + v + d) > 0
    }

    fn h_sign_flags(&self, x: usize, y: usize) -> (bool, bool) {
        let mut pos = false;
        let mut neg = false;
        if x > 0 {
            let i = self.idx(x - 1, y);
            if self.sigma[i] {
                if self.sign[i] {
                    neg = true;
                } else {
                    pos = true;
                }
            }
        }
        if x + 1 < self.w {
            let i = self.idx(x + 1, y);
            if self.sigma[i] {
                if self.sign[i] {
                    neg = true;
                } else {
                    pos = true;
                }
            }
        }
        if pos && neg {
            (false, false)
        } else {
            (pos, neg)
        }
    }

    fn v_sign_flags(&self, x: usize, y: usize) -> (bool, bool) {
        let mut pos = false;
        let mut neg = false;
        if y > 0 {
            let i = self.idx(x, y - 1);
            if self.sigma[i] {
                if self.sign[i] {
                    neg = true;
                } else {
                    pos = true;
                }
            }
        }
        if y + 1 < self.h {
            let i = self.idx(x, y + 1);
            if self.sigma[i] {
                if self.sign[i] {
                    neg = true;
                } else {
                    pos = true;
                }
            }
        }
        if pos && neg {
            (false, false)
        } else {
            (pos, neg)
        }
    }
}

// Context-lookup helpers — same logic as the decoder, duplicated here
// to keep modules free of cross-references to internal helpers.

fn ctxno_zc(h: u32, v: u32, d: u32, orient: Orient) -> usize {
    let h = h.min(2);
    let v = v.min(2);
    let d = d.min(2);
    match orient {
        Orient::Ll => match (h, v, d) {
            (2, _, _) => 8,
            (1, _, _) => {
                if v >= 1 {
                    7
                } else if d >= 1 {
                    6
                } else {
                    5
                }
            }
            (0, 2, _) => 4,
            (0, 1, _) => 3,
            (0, 0, 2) => 2,
            (0, 0, 1) => 1,
            _ => 0,
        },
        Orient::Hl => match (v, h, d) {
            (2, _, _) => 8,
            (1, _, _) => {
                if h >= 1 {
                    7
                } else if d >= 1 {
                    6
                } else {
                    5
                }
            }
            (0, 2, _) => 4,
            (0, 1, _) => 3,
            (0, 0, 2) => 2,
            (0, 0, 1) => 1,
            _ => 0,
        },
        Orient::Hh => {
            let hv = h + v;
            if d >= 3 {
                8
            } else if d == 2 {
                if hv >= 1 {
                    7
                } else {
                    6
                }
            } else if d == 1 {
                if hv == 0 {
                    3
                } else if hv == 1 {
                    4
                } else {
                    5
                }
            } else if hv == 0 {
                0
            } else if hv == 1 {
                1
            } else {
                2
            }
        }
    }
}

fn ctxno_sc(h_pos: bool, h_neg: bool, v_pos: bool, v_neg: bool) -> (usize, u32) {
    let h_contrib: i32 = if h_pos {
        1
    } else if h_neg {
        -1
    } else {
        0
    };
    let v_contrib: i32 = if v_pos {
        1
    } else if v_neg {
        -1
    } else {
        0
    };
    match (h_contrib, v_contrib) {
        (1, 1) => (13, 0),
        (1, 0) => (12, 0),
        (1, -1) => (11, 0),
        (0, 1) => (10, 0),
        (0, 0) => (9, 0),
        (0, -1) => (10, 1),
        (-1, 1) => (11, 1),
        (-1, 0) => (12, 1),
        (-1, -1) => (13, 1),
        _ => unreachable!(),
    }
}

fn ctxno_mag(first_refine: bool, any_neighbour_sig: bool) -> usize {
    if !first_refine {
        CTX_MAG + 2
    } else if any_neighbour_sig {
        CTX_MAG + 1
    } else {
        CTX_MAG
    }
}

fn sigprop_pass_enc(state: &mut EncoderState, mqc: &mut MqcEnc, bpno: i32, orient: Orient) {
    // Reset pi flags for this bitplane.
    for v in state.pi.iter_mut() {
        *v = false;
    }
    let mut y = 0usize;
    while y < state.h {
        let stripe_end = (y + 4).min(state.h);
        for x in 0..state.w {
            for sy in y..stripe_end {
                let idx = state.idx(x, sy);
                if state.sigma[idx] {
                    continue;
                }
                let (h, v, d) = state.hvd_counts(x, sy);
                if h == 0 && v == 0 && d == 0 {
                    continue;
                }
                let zc_ctx = CTX_ZC + ctxno_zc(h, v, d, orient);
                mqc.setcurctx(zc_ctx);
                let sig = state.bit_at(x, sy, bpno);
                mqc.encode(sig);
                dec_trace::emit("sigprop_zc", bpno, x, sy, zc_ctx, sig);
                // Mark sample as sigprop-tested so the cleanup pass
                // doesn't revisit it. Must be set for every sample the
                // sigprop pass probed — even when the decoded bit is 0.
                state.pi[idx] = true;
                if sig != 0 {
                    let (h_pos, h_neg) = state.h_sign_flags(x, sy);
                    let (v_pos, v_neg) = state.v_sign_flags(x, sy);
                    let (sc_ctx, xor) = ctxno_sc(h_pos, h_neg, v_pos, v_neg);
                    mqc.setcurctx(sc_ctx);
                    let sbit_true = if state.sign[idx] { 1u32 } else { 0u32 };
                    let raw = sbit_true ^ xor;
                    mqc.encode(raw);
                    dec_trace::emit("sigprop_sc", bpno, x, sy, sc_ctx, raw);
                    state.sigma[idx] = true;
                }
            }
        }
        y = stripe_end;
    }
}

fn magref_pass_enc(state: &mut EncoderState, mqc: &mut MqcEnc, bpno: i32) {
    let mut y = 0usize;
    while y < state.h {
        let stripe_end = (y + 4).min(state.h);
        for x in 0..state.w {
            for sy in y..stripe_end {
                let idx = state.idx(x, sy);
                if !state.sigma[idx] || state.pi[idx] {
                    continue;
                }
                let any = state.any_neighbour_sig(x, sy);
                let ctx = ctxno_mag(!state.mu[idx], any);
                mqc.setcurctx(ctx);
                let bit = state.bit_at(x, sy, bpno);
                mqc.encode(bit);
                dec_trace::emit("magref", bpno, x, sy, ctx, bit);
                state.mu[idx] = true;
            }
        }
        y = stripe_end;
    }
}

fn cleanup_pass_enc(state: &mut EncoderState, mqc: &mut MqcEnc, bpno: i32, orient: Orient) {
    let mut y = 0usize;
    while y < state.h {
        let stripe_end = (y + 4).min(state.h);
        for x in 0..state.w {
            let full_stripe = stripe_end - y == 4;
            let all_clear = full_stripe
                && (y..stripe_end).all(|sy| {
                    let i = state.idx(x, sy);
                    if state.sigma[i] || state.pi[i] {
                        return false;
                    }
                    let (h, v, d) = state.hvd_counts(x, sy);
                    h == 0 && v == 0 && d == 0
                });
            if all_clear {
                // Find first row (if any) whose `bpno`-bit is set.
                let mut first_sig: Option<usize> = None;
                for sy in y..stripe_end {
                    if state.bit_at(x, sy, bpno) != 0 {
                        first_sig = Some(sy);
                        break;
                    }
                }
                mqc.setcurctx(CTX_AGG);
                let Some(fs) = first_sig else {
                    mqc.encode(0);
                    dec_trace::emit("cleanup_agg", bpno, x, y, CTX_AGG, 0);
                    continue;
                };
                mqc.encode(1);
                dec_trace::emit("cleanup_agg", bpno, x, y, CTX_AGG, 1);
                let run = fs - y;
                mqc.setcurctx(CTX_UNI);
                let hi = ((run >> 1) & 1) as u32;
                let lo = (run & 1) as u32;
                mqc.encode(hi);
                dec_trace::emit("cleanup_uni_hi", bpno, x, y, CTX_UNI, hi);
                mqc.encode(lo);
                dec_trace::emit("cleanup_uni_lo", bpno, x, y, CTX_UNI, lo);
                // Now sample at offset `run` becomes significant.
                let sy = fs;
                let idx = state.idx(x, sy);
                let (h_pos, h_neg) = state.h_sign_flags(x, sy);
                let (v_pos, v_neg) = state.v_sign_flags(x, sy);
                let (sc_ctx, xor) = ctxno_sc(h_pos, h_neg, v_pos, v_neg);
                mqc.setcurctx(sc_ctx);
                let sbit_true = if state.sign[idx] { 1u32 } else { 0u32 };
                let raw = sbit_true ^ xor;
                mqc.encode(raw);
                dec_trace::emit("cleanup_sc_run", bpno, x, sy, sc_ctx, raw);
                state.sigma[idx] = true;
                // Continue per-sample coding for remainder of stripe.
                for sy2 in (fs + 1)..stripe_end {
                    let idx2 = state.idx(x, sy2);
                    if state.sigma[idx2] || state.pi[idx2] {
                        continue;
                    }
                    let (h, v, d) = state.hvd_counts(x, sy2);
                    let zc_ctx = CTX_ZC + ctxno_zc(h, v, d, orient);
                    mqc.setcurctx(zc_ctx);
                    let sig = state.bit_at(x, sy2, bpno);
                    mqc.encode(sig);
                    dec_trace::emit("cleanup_zc_post", bpno, x, sy2, zc_ctx, sig);
                    if sig != 0 {
                        let (h_pos, h_neg) = state.h_sign_flags(x, sy2);
                        let (v_pos, v_neg) = state.v_sign_flags(x, sy2);
                        let (sc_ctx, xor) = ctxno_sc(h_pos, h_neg, v_pos, v_neg);
                        mqc.setcurctx(sc_ctx);
                        let sbit_true = if state.sign[idx2] { 1u32 } else { 0u32 };
                        let raw = sbit_true ^ xor;
                        mqc.encode(raw);
                        dec_trace::emit("cleanup_sc_post", bpno, x, sy2, sc_ctx, raw);
                        state.sigma[idx2] = true;
                    }
                }
                continue;
            }

            // No run — code each remaining sample individually.
            for sy in y..stripe_end {
                let idx = state.idx(x, sy);
                if state.sigma[idx] || state.pi[idx] {
                    continue;
                }
                let (h, v, d) = state.hvd_counts(x, sy);
                let zc_ctx = CTX_ZC + ctxno_zc(h, v, d, orient);
                mqc.setcurctx(zc_ctx);
                let sig = state.bit_at(x, sy, bpno);
                mqc.encode(sig);
                dec_trace::emit("cleanup_zc", bpno, x, sy, zc_ctx, sig);
                if sig != 0 {
                    let (h_pos, h_neg) = state.h_sign_flags(x, sy);
                    let (v_pos, v_neg) = state.v_sign_flags(x, sy);
                    let (sc_ctx, xor) = ctxno_sc(h_pos, h_neg, v_pos, v_neg);
                    mqc.setcurctx(sc_ctx);
                    let sbit_true = if state.sign[idx] { 1u32 } else { 0u32 };
                    let raw = sbit_true ^ xor;
                    mqc.encode(raw);
                    dec_trace::emit("cleanup_sc", bpno, x, sy, sc_ctx, raw);
                    state.sigma[idx] = true;
                }
            }
        }
        y = stripe_end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::t1::decode_cblk;

    /// Encode a tiny block then decode it — the decoded magnitudes
    /// should be non-zero for samples we set, zero otherwise. We don't
    /// demand bit-exact reconstruction inside tier-1 alone because the
    /// decoder's `oneplushalf` deposit adds 0.5 × 2^bpno that we'd
    /// need to subtract. Instead, verify non-zero samples remain
    /// non-zero and signs match.
    #[test]
    fn tier1_roundtrip_nonzero() {
        let w = 8;
        let h = 8;
        let mut samples = vec![0i32; w * h];
        samples[3 * w + 3] = 100;
        samples[4 * w + 4] = -75;
        let band_numbps = 9;
        let enc = encode_cblk(&samples, w, h, band_numbps, Orient::Ll);
        assert!(enc.total_passes > 0);
        // Decode back.
        let bpno = band_numbps + 1 - enc.missing_msb as i32;
        let dec = decode_cblk(
            enc.data.clone(),
            w,
            h,
            bpno,
            enc.total_passes,
            Orient::Ll,
            0,
        );
        // The two non-zero samples should come out non-zero, with the
        // correct sign.
        assert!(
            dec.data[3 * w + 3] > 0,
            "expected positive, got {}",
            dec.data[3 * w + 3]
        );
        assert!(
            dec.data[4 * w + 4] < 0,
            "expected negative, got {}",
            dec.data[4 * w + 4]
        );
    }

    /// All-zero input must produce a decodable "all-zero" block.
    /// Passes can be zero (we don't need to code anything).
    #[test]
    fn tier1_all_zero() {
        let w = 8;
        let h = 8;
        let samples = vec![0i32; w * h];
        let enc = encode_cblk(&samples, w, h, 5, Orient::Ll);
        assert!(
            enc.missing_msb > 0,
            "all-zero cblk must report missing_msb > 0"
        );
    }
}
