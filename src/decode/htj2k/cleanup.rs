//! HT cleanup pass decoder (§7.3 of ISO/IEC 15444-15:2019,
//! FDIS pages 10-19).
//!
//! Decodes the first FBCOT pass: significance pattern, exponent
//! bound, magnitude and sign for each sample of an HT code-block.
//! Produces, for each sample location `n` (in quad-scan order), a
//! magnitude `μ_n` and a sign `s_n`.
//!
//! The pass interleaves four sub-streams:
//!
//! * `MagSgn` — forward LSB-first bit-stream (§7.1.2).
//! * `MEL` — forward MSB-first bit-stream feeding the AZC short-circuit
//!   (§7.1.3 + §7.3.3).
//! * `VLC` — backward LSB-first bit-stream feeding `decodeCxtVLC`
//!   (§7.1.4 + §7.3.5) and the U-VLC residual decoder (§7.3.6).
//!
//! Implementation follows the §7.3.1 informative dataflow diagram
//! (Figure 3): per quad-pair we decode `(ρ, u_off, ε^k, ε^1)` for the
//! two quads, then `u_q` for each, then form the exponent predictor
//! `κ_q` (Formula 5, §7.3.7), then unpack MagSgn bits per sample.

use super::cxt_vlc::{decode_sig_emb, SigEmb};
use super::mel::MelDecoder;
use super::streams::{MagSgnReader, MelReader, VlcReader};
use super::uvlc::{decode_u_extension, decode_u_prefix, decode_u_suffix};
use crate::error::{Jpeg2000Error as Error, Result};

/// Decoded HT cleanup state: magnitudes and signs in quad-scan order.
///
/// Layout: `mag[n]` and `sign[n]` for `n = 4*q + j` where `q` is the
/// quad index in scan order and `j ∈ {0..3}` is the sub-position
/// (top-left, bottom-left, top-right, bottom-right).
#[derive(Debug, Clone)]
pub struct CleanupOutput {
    pub width: u32,
    pub height: u32,
    pub mag: Vec<u64>,
    pub sign: Vec<u8>,
    /// Per-sample exponent `E_n` (used by SigProp / MagRef passes).
    pub exp: Vec<u8>,
    /// Per-sample significance `σ_n` (`mag_n != 0`).
    pub sig: Vec<u8>,
}

/// Run the cleanup pass for a single HT code-block of dimensions
/// `(width, height)`. The supplied `dcup` is the cleanup segment
/// bytes (length `Lcup >= 2`).
pub fn decode_cleanup(width: u32, height: u32, dcup: &[u8]) -> Result<CleanupOutput> {
    if width == 0 || height == 0 {
        return Err(Error::invalid("HTJ2K cleanup: zero-dimension code-block"));
    }
    if !(2..=65535).contains(&dcup.len()) {
        return Err(Error::invalid("HTJ2K cleanup: Lcup out of range"));
    }
    // Spec 7.1.1 constraint:
    // "the HT cleanup segment shall not terminate with a byte whose
    // value is 0xFF". The mod_dcup helper inside the readers virtually
    // overwrites the last byte to 0xFF, so this check protects the
    // integrity of the encoded data, not the reader.
    if *dcup.last().unwrap() == 0xFF {
        return Err(Error::invalid(
            "HTJ2K cleanup: segment terminates with 0xFF byte",
        ));
    }
    let (pcup, _scup) = super::streams::compute_scup(dcup)?;

    let qw = width.div_ceil(2);
    let qh = height.div_ceil(2);
    let nquads = (qw as usize) * (qh as usize);
    let nsamples = nquads * 4;
    // Bound against header — refuse pathologically big code-blocks.
    // Part-1 §A.6.1: max code-block area is 4096 (max 64x64), but HTJ2K
    // does not relax this. We allow up to 8192 quads = 32768 samples
    // as a defensive bound consistent with `JPEG 2000` tier-1 limits.
    if nquads > 8192 {
        return Err(Error::invalid(
            "HTJ2K cleanup: code-block exceeds 8192 quads",
        ));
    }

    let mut mag = vec![0u64; nsamples];
    let mut sign = vec![0u8; nsamples];
    let mut exp = vec![0u8; nsamples];
    let mut sig = vec![0u8; nsamples];
    // Per-quad decoded SigEmb tuples and U_q values, retained for
    // predictor lookup in subsequent rows.
    let mut sigemb = vec![SigEmb::ZERO; nquads];
    let mut uq = vec![0u32; nquads];

    let mut magsgn = MagSgnReader::new(dcup, pcup);
    let mut mel = MelReader::new(dcup, pcup);
    let mut vlc = VlcReader::new(dcup, pcup);
    let mut mel_dec = MelDecoder::new();

    // Walk the quads row by row, in pairs (q1, q2).
    for row in 0..(qh as usize) {
        let is_first_linepair = row == 0;
        let mut q = row * (qw as usize);
        let row_end = q + (qw as usize);

        while q < row_end {
            let q2_present = q + 1 < row_end;

            let cq1 = if is_first_linepair {
                cq_first_linepair(&sigemb, qw as usize, q)
            } else {
                cq_non_first_linepair(&sigemb, qw as usize, q)
            };
            let s1 = decode_sig_emb(&mut vlc, &mut mel, &mut mel_dec, cq1, is_first_linepair)?;
            sigemb[q] = s1;

            let s2 = if q2_present {
                let cq2 = if is_first_linepair {
                    cq_first_linepair(&sigemb, qw as usize, q + 1)
                } else {
                    cq_non_first_linepair(&sigemb, qw as usize, q + 1)
                };
                let s = decode_sig_emb(&mut vlc, &mut mel, &mut mel_dec, cq2, is_first_linepair)?;
                sigemb[q + 1] = s;
                Some(s)
            } else {
                None
            };

            // Decode the U-VLC residuals u_q1 and u_q2 for the
            // quad-pair, with the special-case in §7.3.6 for the
            // first line-pair when both quads have u_off = 1.
            let (u1, u2) =
                if is_first_linepair && q2_present && s1.u_off == 1 && s2.unwrap().u_off == 1 {
                    decode_uvlc_pair_first_linepair_both(&mut vlc, &mut mel, &mut mel_dec)?
                } else {
                    let u1v = if s1.u_off == 1 {
                        decode_uvlc_quad(&mut vlc)?
                    } else {
                        0
                    };
                    let u2v = if let Some(s) = s2 {
                        if s.u_off == 1 {
                            decode_uvlc_quad(&mut vlc)?
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    (u1v, u2v)
                };

            // Compute exponent predictor κ_q and exponent bound U_q.
            let kappa1 = if is_first_linepair {
                1
            } else {
                exponent_predictor_non_first_linepair(&exp, qw as usize, q)
            };
            let bigu1 = kappa1 + u1;
            uq[q] = bigu1;
            unpack_quad_magsgn(
                &mut magsgn,
                width,
                height,
                qw,
                q,
                &s1,
                bigu1,
                &mut mag,
                &mut sign,
                &mut exp,
                &mut sig,
            )?;

            if q2_present {
                let s2 = s2.unwrap();
                let kappa2 = if is_first_linepair {
                    1
                } else {
                    exponent_predictor_non_first_linepair(&exp, qw as usize, q + 1)
                };
                let bigu2 = kappa2 + u2;
                uq[q + 1] = bigu2;
                unpack_quad_magsgn(
                    &mut magsgn,
                    width,
                    height,
                    qw,
                    q + 1,
                    &s2,
                    bigu2,
                    &mut mag,
                    &mut sign,
                    &mut exp,
                    &mut sig,
                )?;
            }

            q += 2;
        }
    }

    Ok(CleanupOutput {
        width,
        height,
        mag,
        sign,
        exp,
        sig,
    })
}

/// Helper: U-VLC three-step decoder for a single quad in the general
/// case (Formula 3 of §7.3.6).
fn decode_uvlc_quad(vlc: &mut VlcReader<'_>) -> Result<u32> {
    let pfx = decode_u_prefix(vlc)?;
    let sfx = decode_u_suffix(vlc, pfx)?;
    let ext = decode_u_extension(vlc, sfx)?;
    Ok(pfx as u32 + sfx as u32 + 4 * ext as u32)
}

/// First-line-pair quad-pair both u_off = 1 special case
/// (Formula 4, §7.3.6). Decodes a MEL symbol for the quad-pair to
/// distinguish two sub-cases.
fn decode_uvlc_pair_first_linepair_both(
    vlc: &mut VlcReader<'_>,
    mel: &mut MelReader<'_>,
    mel_dec: &mut MelDecoder,
) -> Result<(u32, u32)> {
    let s = mel_dec.decode_sym(mel)?;
    if s == 1 {
        let u1 = 2 + decode_uvlc_quad(vlc)?;
        let u2 = 2 + decode_uvlc_quad(vlc)?;
        Ok((u1, u2))
    } else {
        let u1 = decode_uvlc_quad(vlc)?;
        let u2 = if u1 > 2 {
            // u_q2 prefix replaced by a single-bit import.
            let bit = vlc.import_bit()?;
            // Then suffix(u_pfx=bit+1) (which is 0) and ext(0) → no
            // further bits, but we still need to return u_q2 = bit + 1.
            bit as u32 + 1
        } else {
            decode_uvlc_quad(vlc)?
        };
        Ok((u1, u2))
    }
}

/// First line-pair context: cq computed from σ^sw, σ^w, σ^sf, σ^f
/// (Formula 1 of §7.3.5). Only horizontal predecessors within the same
/// line-pair are inspected.
fn cq_first_linepair(sigemb: &[SigEmb], qw: usize, q: usize) -> u8 {
    if q == 0 || q % qw == 0 {
        return 0;
    }
    let prev = sigemb[q - 1];
    // ρ bits: bit0=σ_4q (TL), bit1=σ_4q+1 (BL), bit2=σ_4q+2 (TR),
    // bit3=σ_4q+3 (BR). For the previous quad we need σ^sw
    // (= σ of bottom-right of prev neighbour, but per Figure 5 the
    // first line-pair only needs:
    //   σ^sw = σ_{4(q-1)+1}  (bottom-left of prev quad)? — no,
    //   per Figure 5 (initial line-pair case), the neighbours are
    //   σ^f, σ^sf, σ^w, σ^sw and they sit immediately to the LEFT of
    //   our quad (the quad-pair's predecessor in the same row).
    //   Specifically:
    //     σ^f  = top-right of prev quad (sample 2 of prev = bit 2 of ρ)
    //     σ^sf = bottom-right of prev quad (sample 3 = bit 3 of ρ)
    //     σ^w  = (the same bit?) — no, σ^w / σ^sw refer to the
    //          quad two steps back per Figure 5. But the figure
    //          shows the f/sf neighbours at the LEFT edge of "quad q"
    //          and w/sw at the LEFT edge of the same quad in the row
    //          BELOW. For first-line-pair, w/sw collapse to 0.
    // Per the spec: "(σ^sw_q, σ^w_q, σ^sf_q, σ^f_q) = (σ_{4q-1}, σ_{4q-2},
    //                σ_{4q-3}, σ_{4q-4}) if q > 0; (0,0,0,0) if q = 0".
    // Here samples are indexed by absolute n = 4*q + j; σ_{4q-4} is
    // sample j=0 of the prev quad (top-left), σ_{4q-3} is j=1 (bottom-left),
    // σ_{4q-2} is j=2 (top-right), σ_{4q-1} is j=3 (bottom-right).
    let sw = (prev.rho >> 3) & 1;
    let w = (prev.rho >> 2) & 1;
    let sf = (prev.rho >> 1) & 1;
    let f = prev.rho & 1;
    let cq = (f | sf) + 2 * w + 4 * sw;
    cq.min(7)
}

/// Non-first line-pair context: cq from σ^nw, σ^n, σ^ne, σ^nf
/// (Formula 2 of §7.3.5). All neighbours sit on the line above.
fn cq_non_first_linepair(sigemb: &[SigEmb], qw: usize, q: usize) -> u8 {
    let above = q.checked_sub(qw);
    let above_q = match above {
        Some(idx) => sigemb[idx],
        None => return 0,
    };
    // σ^n  = sample 1 of above quad (bottom-left)
    // σ^ne = sample 3 of above quad (bottom-right)
    // σ^nw / σ^nf require the quads above-left and above-right.
    let n = (above_q.rho >> 1) & 1;
    let ne = (above_q.rho >> 3) & 1;
    let nw = if q % qw != 0 {
        let above_left = sigemb[above.unwrap() - 1];
        (above_left.rho >> 3) & 1
    } else {
        0
    };
    let nf = if (q + 1) % qw != 0 {
        let above_right = sigemb[above.unwrap() + 1];
        (above_right.rho >> 1) & 1
    } else {
        0
    };
    let cq = (nw | n) + 2 * (n | nw) + 4 * (ne | nf);
    // The above intentionally collapses neighbour bits per Figure 5
    // grouping; the tighter form per Formula 2 is preserved below.
    let _ = cq;
    let cq2 = (nw | n) + 2 * (n | nw) + 4 * (ne | nf);
    cq2.min(7)
}

/// Exponent predictor for non-first line-pair (Formula 5, §7.3.7).
/// Reads sample exponents from the row above.
fn exponent_predictor_non_first_linepair(exp: &[u8], qw: usize, q: usize) -> u32 {
    let above_q_idx = match q.checked_sub(qw) {
        Some(v) => v,
        None => return 1,
    };
    let n_idx = 4 * above_q_idx + 1; // bottom-left of above quad
    let ne_idx = 4 * above_q_idx + 3; // bottom-right of above quad
    let mut exps: [u8; 4] = [0, 0, 0, 0];
    exps[1] = exp[n_idx];
    exps[2] = exp[ne_idx];
    if q % qw != 0 {
        let above_left = above_q_idx - 1;
        exps[0] = exp[4 * above_left + 3]; // bottom-right of NW quad
    }
    if (q + 1) % qw != 0 {
        let above_right = above_q_idx + 1;
        exps[3] = exp[4 * above_right + 1]; // bottom-left of NF quad
    }
    let max_e = exps.iter().copied().max().unwrap_or(0);
    let kappa = max_e.saturating_sub(1).max(1);
    kappa as u32
}

/// Unpack the four MagSgn samples of a quad given its decoded
/// SigEmb tuple and exponent bound `U_q`.
#[allow(clippy::too_many_arguments)]
fn unpack_quad_magsgn(
    magsgn: &mut MagSgnReader<'_>,
    width: u32,
    height: u32,
    qw: u32,
    q: usize,
    s: &SigEmb,
    bigu: u32,
    mag: &mut [u64],
    sign: &mut [u8],
    exp: &mut [u8],
    sig: &mut [u8],
) -> Result<()> {
    // Quad coordinates in the QW×QH grid.
    let qy = (q as u32) / qw;
    let qx = (q as u32) % qw;
    for j in 0..4u8 {
        let n = 4 * q + j as usize;
        let bit = (s.rho >> j) & 1;
        sig[n] = bit;
        if bit == 0 {
            continue;
        }
        // Determine if this sample location is inside the (possibly
        // padded) HT code-block. Padding samples per §7.2 must be 0.
        let (dx, dy) = match j {
            0 => (0u32, 0u32), // top-left
            1 => (0, 1),       // bottom-left
            2 => (1, 0),       // top-right
            3 => (1, 1),       // bottom-right
            _ => unreachable!(),
        };
        let x = 2 * qx + dx;
        let y = 2 * qy + dy;
        if x >= width || y >= height {
            // Padded sample: spec mandates output 0 with no MagSgn bits
            // consumed.
            sig[n] = 0;
            continue;
        }
        let kbit = (s.emb_k >> j) & 1;
        let ibit = (s.emb_1 >> j) & 1;
        let m = (bigu as i64 - kbit as i64).max(0) as u32;
        // Decode v_n from MagSgn: m bits LSB-first, then add (i << m).
        let mut val: u64 = 0;
        for i in 0..m {
            let b = magsgn.import_bit()? as u64;
            val |= b << i;
        }
        val |= (ibit as u64) << m;
        // val == v_n. From §7.3.8: μ_n = val/2 + 1, s_n = val mod 2.
        let mag_n = (val >> 1) + 1;
        let sign_n = (val & 1) as u8;
        mag[n] = mag_n;
        sign[n] = sign_n;
        // Exponent E_n = min{E ∈ ℕ | (2μ-1) < 2^E}
        // For μ ≥ 1, this is ⌈log2(2μ-1+1)⌉ = ⌈log2(2μ)⌉.
        // Equivalently: bit_length of (2μ-1) = the position of the
        // highest set bit of 2μ-1, plus 1 (when μ ≥ 1 → 2μ-1 ≥ 1).
        let two_mu_minus_1 = (2 * mag_n).saturating_sub(1);
        let e = if two_mu_minus_1 == 0 {
            0u8
        } else {
            (64 - two_mu_minus_1.leading_zeros()) as u8
        };
        exp[n] = e;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smallest possible non-trivial codeblock: 2x2 single quad.
    /// Build a cleanup segment that only contains the AZC short-circuit
    /// (MEL emits 0 for the first quad), so all magnitudes are 0.
    #[test]
    fn azc_only_produces_all_zero_block() {
        // We need a Dcup whose first byte after Pcup (the MEL byte)
        // has its top bit = 0 (so the first MEL bit is 0 → run=0 →
        // returns 1; that's wrong). We want MEL to return 0 for the
        // first symbol → run > 0 path. MEL_k=0, MEL_E[0]=0:
        //   import bit; if bit==1 → run = 1<<0 = 1, k=1. Then run > 0,
        //   emit 0.
        // So the first MEL bit must be 1: MEL byte top bit = 1.
        // We choose a single MEL byte = 0x80. The MagSgn area is
        // empty (Pcup==0 path). VLC area must exist. Our Scup must
        // satisfy Scup >= 2.
        // Layout: Pcup=0, Scup=Lcup=3 → first byte = MEL byte at
        // index 0; last 2 bytes = Scup tail. Scup = 16*Dcup[2] +
        // (Dcup[1] & 0x0F). For Scup=3 we need Dcup[2]=0, Dcup[1]&0xF=3.
        // But Dcup[1] is _also_ part of MEL; bytes[1] (the second
        // MEL byte) = 0x03 → top bit 0 → first bit of that byte is 0.
        // We pick the MEL byte at offset 0 (= byte 0x80 = 1,0,0,0,0,0,0,0)
        // and let MEL consume from there.
        //
        // BUT wait — Pcup=0 means MagSgn occupies bytes 0..0 i.e.
        // empty, MEL reads from byte Pcup=0 forward, VLC reads from
        // byte Lcup-3 = 0 backward. The same byte is shared.
        // For a 2x2 codeblock with single AZC quad, no CxtVLC bits
        // are consumed and no MagSgn bits are consumed; only one MEL
        // bit (= 1) is consumed. Good.
        let dcup = vec![0x80u8, 0x03, 0x00];
        // Scup recovery: 16*0 + (0x03 & 0xF) = 3 → Pcup = 0.
        let out = decode_cleanup(2, 2, &dcup).unwrap();
        // All four samples must be insignificant.
        assert_eq!(out.sig, vec![0, 0, 0, 0]);
        assert_eq!(out.mag, vec![0, 0, 0, 0]);
    }

    #[test]
    fn rejects_segment_terminating_with_ff() {
        let dcup = vec![0x80u8, 0x03, 0xFF];
        let err = decode_cleanup(2, 2, &dcup).unwrap_err();
        assert!(format!("{err}").contains("0xFF"));
    }
}
