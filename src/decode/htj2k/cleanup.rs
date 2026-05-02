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
use oxideav_core::{Error, Result};

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
///
/// `p_shift` is the bit-plane shift `p = M_b + 1 - missing_msbs` where
/// `M_b = guard_bits + ε_b - 1` per ISO 15444-1 Annex E. Per the
/// OpenJPEG/OpenJPH HTJ2K block decoder (round 6.5 alignment), each
/// significant sample's reconstructed integer coefficient at the band's
/// raw scale (the "31-bit oneplushalf" form) is
/// `((v_n | 1) + 2) << (p_shift - 1)` (sign separate). The downstream
/// dequantiser then divides by 2 to recover the band-LSB integer value
/// — equivalent to Part-1 t1's `oneplushalf` `/2` step.
///
/// When `p_shift = 0` the helper falls back to the **legacy** μ_n =
/// `(val >> 1) + 1` form. This keeps the existing SigProp / MagRef unit
/// tests working: those exercise the cleanup output as a raw bit-grid
/// (not an integer band coefficient), and the formula difference cancels
/// out for them.
#[allow(dead_code)]
pub fn decode_cleanup(width: u32, height: u32, dcup: &[u8]) -> Result<CleanupOutput> {
    decode_cleanup_with_shift(width, height, dcup, 0)
}

/// Like [`decode_cleanup`] but takes the bit-plane shift `p_shift`
/// computed by the tier-2 walker from the band's `M_b` and the
/// codeblock's `missing_msbs`.
pub fn decode_cleanup_with_shift(
    width: u32,
    height: u32,
    dcup: &[u8],
    p_shift: u32,
) -> Result<CleanupOutput> {
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
            // quad-pair. Per Figure 4 of §7.3.4, the U-VLC bits for
            // the two quads are *interleaved*: prefix(q1), prefix(q2),
            // suffix(q1), suffix(q2), ext(q1), ext(q2). The first
            // line-pair has a special-cased handling (Formula 4) when
            // both quads have u_off = 1.
            let s2_u_off = s2.map(|s| s.u_off).unwrap_or(0);
            let (u1, u2) = if is_first_linepair && q2_present && s1.u_off == 1 && s2_u_off == 1 {
                decode_uvlc_pair_first_linepair_both(&mut vlc, &mut mel, &mut mel_dec)?
            } else {
                decode_uvlc_pair_interleaved(&mut vlc, s1.u_off == 1, s2_u_off == 1 && q2_present)?
            };

            // Compute exponent predictor κ_q and exponent bound U_q.
            let kappa1 = if is_first_linepair {
                1
            } else {
                exponent_predictor_non_first_linepair(&exp, qw as usize, q, s1.rho)
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
                p_shift,
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
                    exponent_predictor_non_first_linepair(&exp, qw as usize, q + 1, s2.rho)
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
                    p_shift,
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

/// Quad-pair U-VLC decoding with the prefix/suffix/extension
/// interleave mandated by Figure 4 of §7.3.4 (general case): the
/// prefix bits for both quads come first, then the suffix bits for
/// both, then the extension bits for both. Quads with `u_off = 0`
/// contribute no bits and decode to `u = 0`.
fn decode_uvlc_pair_interleaved(
    vlc: &mut VlcReader<'_>,
    q1_active: bool,
    q2_active: bool,
) -> Result<(u32, u32)> {
    let pfx1 = if q1_active { decode_u_prefix(vlc)? } else { 0 };
    let pfx2 = if q2_active { decode_u_prefix(vlc)? } else { 0 };
    let sfx1 = if q1_active {
        decode_u_suffix(vlc, pfx1)?
    } else {
        0
    };
    let sfx2 = if q2_active {
        decode_u_suffix(vlc, pfx2)?
    } else {
        0
    };
    let ext1 = if q1_active {
        decode_u_extension(vlc, sfx1)?
    } else {
        0
    };
    let ext2 = if q2_active {
        decode_u_extension(vlc, sfx2)?
    } else {
        0
    };
    let u1 = if q1_active {
        pfx1 as u32 + sfx1 as u32 + 4 * ext1 as u32
    } else {
        0
    };
    let u2 = if q2_active {
        pfx2 as u32 + sfx2 as u32 + 4 * ext2 as u32
    } else {
        0
    };
    Ok((u1, u2))
}

/// First-line-pair quad-pair both u_off = 1 special case
/// (Formula 4, §7.3.6). Decodes a MEL symbol for the quad-pair to
/// distinguish two sub-cases. Both branches must respect the
/// quad-pair interleaving order from Figure 4 (prefix(q1), prefix(q2),
/// suffix(q1), suffix(q2), ext(q1), ext(q2)) — except for the
/// `s_mel = 0, u_q1 > 2` branch where the second quad's prefix is
/// replaced by a single-bit import that fully determines u_q2.
fn decode_uvlc_pair_first_linepair_both(
    vlc: &mut VlcReader<'_>,
    mel: &mut MelReader<'_>,
    mel_dec: &mut MelDecoder,
) -> Result<(u32, u32)> {
    let s = mel_dec.decode_sym(mel)?;
    if s == 1 {
        // Both quads use Formula 4 with the +2 baseline. Decode
        // prefix/suffix/extension bits in interleaved Figure-4 order.
        let pfx1 = decode_u_prefix(vlc)?;
        let pfx2 = decode_u_prefix(vlc)?;
        let sfx1 = decode_u_suffix(vlc, pfx1)?;
        let sfx2 = decode_u_suffix(vlc, pfx2)?;
        let ext1 = decode_u_extension(vlc, sfx1)?;
        let ext2 = decode_u_extension(vlc, sfx2)?;
        let u1 = 2 + pfx1 as u32 + sfx1 as u32 + 4 * ext1 as u32;
        let u2 = 2 + pfx2 as u32 + sfx2 as u32 + 4 * ext2 as u32;
        Ok((u1, u2))
    } else {
        // s_mel = 0: q1 decoded by Formula 3. q2 depends on u_q1:
        //   * u_q1 > 2: q2 prefix replaced by single-bit import; that
        //     bit determines u_q2 = bit + 1 (no suffix/extension bits).
        //   * u_q1 ≤ 2: q2 decoded by Formula 3 too, with proper
        //     Figure-4 interleaving relative to q1.
        // Decode q1's prefix first, so we know its full u_q1 value
        // before fetching q2's prefix.
        let pfx1 = decode_u_prefix(vlc)?;
        // We need the suffix/ext bits of q1 to know u_q1 numerically;
        // but Figure 4 says prefix(q1), prefix(q2) come before any
        // suffix bits. Work around: per Formula (4) text, "where u_q1
        // > 2 the U-VLC prefix decoding step for u_q2 is replaced by
        // using importVLCBit". The "U-VLC prefix decoding step" is
        // the slot that would otherwise consume up to 3 bits — when
        // u_q1 > 2, only 1 bit is imported in that slot. The check
        // `u_q1 > 2` is equivalent to `pfx1 ≥ 3` (since the prefix
        // alone determines whether u ≤ 2).
        if pfx1 >= 3 {
            // Single-bit import takes the place of q2's prefix.
            let q2_bit = vlc.import_bit()?;
            // Now finish q1's decoding (suffix + extension).
            let sfx1 = decode_u_suffix(vlc, pfx1)?;
            let ext1 = decode_u_extension(vlc, sfx1)?;
            let u1 = pfx1 as u32 + sfx1 as u32 + 4 * ext1 as u32;
            let u2 = q2_bit as u32 + 1;
            Ok((u1, u2))
        } else {
            // u_q1 ≤ 2 (i.e. pfx1 ∈ {1, 2}, no suffix/ext for q1):
            // q2 decoded by Formula 3, interleaved with q1.
            let pfx2 = decode_u_prefix(vlc)?;
            let sfx1 = decode_u_suffix(vlc, pfx1)?; // = 0 since pfx1 < 3
            let sfx2 = decode_u_suffix(vlc, pfx2)?;
            let ext1 = decode_u_extension(vlc, sfx1)?; // = 0
            let ext2 = decode_u_extension(vlc, sfx2)?;
            let u1 = pfx1 as u32 + sfx1 as u32 + 4 * ext1 as u32;
            let u2 = pfx2 as u32 + sfx2 as u32 + 4 * ext2 as u32;
            Ok((u1, u2))
        }
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

/// Non-first line-pair context: cq from σ^nw, σ^n, σ^ne, σ^nf,
/// σ^w and σ^sw (Formula 2 of §7.3.5):
///
/// `c_q = (σ^nw | σ^n) + 2 · (σ^w | σ^sw) + 4 · (σ^ne | σ^nf)`
///
/// The `nw, n, ne, nf` neighbours come from the row of quads above
/// (per Figure 5, left side); the `w, sw` neighbours come from the
/// quad immediately to the left in the SAME row (Figure 5 also draws
/// the `w, sw` column adjacent to "quad q" in the non-initial case).
/// Indexing follows the same per-quad sample layout as the initial
/// line-pair: `σ^w_q = σ_{4(q-1)+2}` (top-right of left quad) and
/// `σ^sw_q = σ_{4(q-1)+3}` (bottom-right of left quad).
fn cq_non_first_linepair(sigemb: &[SigEmb], qw: usize, q: usize) -> u8 {
    let above = q.checked_sub(qw);
    let above_q = match above {
        Some(idx) => sigemb[idx],
        None => return 0,
    };
    // σ^n  = sample 1 of above quad (bottom-left of N quad).
    // σ^ne = sample 3 of above quad (bottom-right of N quad).
    let n = (above_q.rho >> 1) & 1;
    let ne = (above_q.rho >> 3) & 1;
    // σ^nw = sample 3 of above-left quad (bottom-right of NW quad)
    //         when q is not in the first column.
    let nw = if q % qw != 0 {
        let above_left = sigemb[above.unwrap() - 1];
        (above_left.rho >> 3) & 1
    } else {
        0
    };
    // σ^nf = sample 1 of above-right quad (bottom-left of NF quad)
    //         when q is not in the last column.
    let nf = if (q + 1) % qw != 0 {
        let above_right = sigemb[above.unwrap() + 1];
        (above_right.rho >> 1) & 1
    } else {
        0
    };
    // σ^w  = sample 2 of left quad (top-right) when q has a left
    //         neighbour in the same row.
    // σ^sw = sample 3 of left quad (bottom-right).
    let (w, sw) = if q % qw != 0 {
        let left = sigemb[q - 1];
        ((left.rho >> 2) & 1, (left.rho >> 3) & 1)
    } else {
        (0, 0)
    };
    let cq = (nw | n) + 2 * (w | sw) + 4 * (ne | nf);
    cq.min(7)
}

/// Exponent predictor for non-first line-pair (Formula 5 + 6, §7.3.7).
/// Reads sample exponents from the row above and applies the
/// significance-pattern gate `γ_q`:
///
/// `κ_q = max{1, γ_q · (max{E^nw, E^n, E^ne, E^nf} - 1)}`
///
/// where `γ_q = 0` if `ρ_q ∈ {0, 1, 2, 4, 8}` (i.e., at most one
/// significant sample in the quad), `γ_q = 1` otherwise.
fn exponent_predictor_non_first_linepair(exp: &[u8], qw: usize, q: usize, rho: u8) -> u32 {
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
    // γ_q = 0 when ρ_q has zero or one set bit (∈ {0,1,2,4,8}); else 1.
    let gamma_is_zero = matches!(rho, 0 | 1 | 2 | 4 | 8);
    let predictor = if gamma_is_zero {
        0u32
    } else {
        max_e.saturating_sub(1) as u32
    };
    predictor.max(1)
}

/// Unpack the four MagSgn samples of a quad given its decoded
/// SigEmb tuple and exponent bound `U_q`.
///
/// `p_shift` controls magnitude reconstruction: when `p_shift > 0` we
/// follow the OpenJPEG/OpenJPH formula
/// `M_n = ((v_n | 1) + 2) << (p_shift - 1)` and the **band-LSB integer
/// magnitude** is `M_n / 2`. When `p_shift == 0` we use the legacy
/// `μ_n = (val >> 1) + 1` reading (preserved so per-bit unit tests on
/// SigProp / MagRef keep working — those don't care about absolute
/// scale).
#[allow(clippy::too_many_arguments)]
fn unpack_quad_magsgn(
    magsgn: &mut MagSgnReader<'_>,
    width: u32,
    height: u32,
    qw: u32,
    q: usize,
    s: &SigEmb,
    bigu: u32,
    p_shift: u32,
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
        // val == v_n_raw. Sign is bit 0 of the m bits read from MagSgn
        // (per OpenJPEG `val = ms_val << 31`); ibit lives at bit `m`.
        let sign_n = (val & 1) as u8;
        let mag_n = if p_shift == 0 {
            // Legacy bin-centre form (used by SigProp/MagRef unit tests):
            // μ_n = (val >> 1) + 1.
            (val >> 1) + 1
        } else {
            // OpenJPEG/OpenJPH HTJ2K block decoder formula:
            //   v_n = val | 1                 // force LSB to 1 (bin centre)
            //   M_n_raw = (v_n + 2) << (p-1)  // 31-bit "oneplushalf" form
            //   band_int = M_n_raw / 2        // /2 dequant (Part-1 lossless convention)
            // Combining: band_int = ((val | 1) + 2) << (p_shift - 2)
            // when p_shift >= 2; for p_shift == 1, band_int = ((val | 1)
            // + 2) >> 1, which we express as a saturated-zero shift.
            let v_n = val | 1;
            let m_n_raw = (v_n + 2) << (p_shift - 1);
            m_n_raw >> 1
        };
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

    /// Verify Formula 2 of §7.3.5 produces a context value in 0..=7 for
    /// every combination of neighbouring rho-bit configurations. The
    /// round-4 implementation collapsed the formula's `(σ^w | σ^sw)`
    /// term down to `(σ^n | σ^nw)`, which only ever produced contexts
    /// 0, 3, or 7 — round 5 restored the left-neighbour read so the
    /// full 0..=7 range is reachable.
    #[test]
    fn cq_non_first_linepair_covers_full_context_range() {
        use std::collections::HashSet;
        // Build a 3-quad-wide × 2-quad-tall sigemb grid so the
        // middle quad of row 1 has populated NW, N, NE, NF, W, SW
        // neighbours. We sweep every combination of those six bits.
        let qw = 3usize;
        let mut seen: HashSet<u8> = HashSet::new();
        for bits in 0u8..64 {
            // Encode neighbour significance into rho values for the
            // six neighbours. Each neighbour samples a particular bit
            // of the 4-sample rho pattern.
            let nw_bit = bits & 1;
            let n_bit = (bits >> 1) & 1;
            let ne_bit = (bits >> 2) & 1;
            let nf_bit = (bits >> 3) & 1;
            let w_bit = (bits >> 4) & 1;
            let sw_bit = (bits >> 5) & 1;
            let mut sigemb = vec![SigEmb::ZERO; 6];
            // q=0 (qx=0, qy=0) is NW of q=4 — its sample 3 (rho bit 3)
            // is σ^nw of q=4.
            sigemb[0].rho = nw_bit << 3;
            // q=1 (qx=1, qy=0) is N of q=4 — its sample 1 + sample 3
            // are σ^n / σ^ne.
            sigemb[1].rho = (n_bit << 1) | (ne_bit << 3);
            // q=2 (qx=2, qy=0) is NE of q=4 — its sample 1 is σ^nf.
            sigemb[2].rho = nf_bit << 1;
            // q=3 (qx=0, qy=1) is W of q=4 — its sample 2 + sample 3
            // are σ^w / σ^sw.
            sigemb[3].rho = (w_bit << 2) | (sw_bit << 3);
            let cq = cq_non_first_linepair(&sigemb, qw, 4);
            assert!(cq <= 7, "context out of range: {cq} for bits {bits:#06b}");
            seen.insert(cq);
        }
        assert_eq!(
            seen.len(),
            8,
            "expected all 8 contexts (0..=7) to be reachable, saw {seen:?}"
        );
    }
}
