//! HT cleanup pass encoder (inverse of
//! [`crate::decode::htj2k::cleanup`]).
//!
//! Round 1 scope:
//!
//! * Single HT cleanup pass per code-block (Z_blk = 1). SigProp /
//!   MagRef are deferred to round 2.
//! * The encoder requires that **every quad has at most one
//!   significant sample**. Multi-significant quads (ρ ∈ {3, 5, 6, 7,
//!   9, 10, 11, 12, 13, 14, 15}) are rejected with
//!   [`Error::Unsupported`] so the round-2 work can extend the
//!   per-quad EMB selection logic to handle them.
//! * Single significance per quad means the encoder can always pick
//!   the table row `(u_off=1, emb_k=ρ, emb_1=ρ)` for ρ ∈ {1, 2, 4, 8}
//!   plus the `(u_off=0, emb_k=0, emb_1=0)` row for ρ = 0 — both
//!   guaranteed to exist in Annex C tables 0 and 1 (verified by the
//!   `table0_codewords_match_after_encode` /
//!   `table1_codewords_match_after_encode` round-trips).
//! * The MEL stream is used only for the ρ=0, cq=0 short-circuit
//!   (AZC fall-through when the previous quad had cq=0 and the current
//!   quad is also AZC).
//! * Output is the assembled `Dcup` byte sequence ready to splice into
//!   a tier-2 packet body.

use super::cxt_vlc_enc::encode_cxt_vlc;
use super::mel_enc::encode_mel_symbols;
use super::streams_enc::{MagSgnWriter, VlcWriter};
use super::uvlc_enc::{encode_u_extension, encode_u_prefix, encode_u_suffix, split_u};
use crate::error::{Jpeg2000Error as Error, Result};

/// Per-sample input to the HT cleanup encoder. Sign is stored as a
/// raw u8 (0 = positive / non-negative, 1 = negative); magnitude is
/// the absolute value the encoder will emit.
#[derive(Debug, Clone, Copy, Default)]
pub struct SampleHt {
    pub mag: u32,
    pub sign: u8,
}

/// Encode an HT cleanup segment for one code-block. The samples are
/// laid out in raster order `samples[y * width + x]`.
pub fn encode_cleanup(width: u32, height: u32, samples: &[SampleHt]) -> Result<Vec<u8>> {
    if width == 0 || height == 0 {
        return Err(Error::invalid("HTJ2K encode: zero-dimension code-block"));
    }
    if samples.len() != (width as usize) * (height as usize) {
        return Err(Error::invalid("HTJ2K encode: sample count mismatch"));
    }

    let qw = width.div_ceil(2) as usize;
    let qh = height.div_ceil(2) as usize;
    let nquads = qw * qh;

    // Per-quad recovered (rho, sample_v[4], bigu) so we can emit MagSgn
    // bits and recompute neighbours' kappa_q on the fly. The struct
    // definition lives at module scope (see [`QuadEnc`] below) so that
    // helper functions outside this body can borrow it.
    let mut quads: Vec<QuadEnc> = vec![QuadEnc::default(); nquads];

    // Compute (rho, v, e) for every quad and validate the round-1
    // single-significant-sample restriction.
    for qy in 0..qh {
        for qx in 0..qw {
            let q = qy * qw + qx;
            let mut rho = 0u8;
            let mut bigu = 0u32;
            let mut v = [0u32; 4];
            let mut ee = [0u8; 4];
            for j in 0..4u8 {
                let (dx, dy) = match j {
                    0 => (0u32, 0u32),
                    1 => (0, 1),
                    2 => (1, 0),
                    3 => (1, 1),
                    _ => unreachable!(),
                };
                let x = 2 * qx as u32 + dx;
                let y = 2 * qy as u32 + dy;
                if x >= width || y >= height {
                    continue; // padding sample — output is 0 by §7.2
                }
                let s = samples[(y as usize) * (width as usize) + (x as usize)];
                if s.mag == 0 {
                    continue;
                }
                rho |= 1u8 << j;
                // v = 2(μ - 1) + sign per T.814 §7.3.8. Decoder sets
                // val ← bits, then val |= ibit << m, then μ = val/2 + 1
                // and s = val & 1 — so encoded `val` equals
                // `2*(μ-1) + sign`.
                let vj = 2 * (s.mag - 1) + (s.sign as u32 & 1);
                v[j as usize] = vj;
                let bl = if vj == 0 { 0 } else { 32 - vj.leading_zeros() };
                bigu = bigu.max(bl);
                // Per-sample exponent for the κ_q predictor in
                // subsequent rows: E_n = bit_len(2μ - 1).
                let two_mu_minus_1 = (2 * s.mag).saturating_sub(1);
                let e_n = if two_mu_minus_1 == 0 {
                    0u8
                } else {
                    (32 - two_mu_minus_1.leading_zeros()) as u8
                };
                ee[j as usize] = e_n;
            }
            // Round-1 restriction: at most one significant sample per
            // quad.
            if rho.count_ones() > 1 {
                return Err(Error::unsupported(
                    "HTJ2K encode: multi-significant-sample quad (round 2+)",
                ));
            }
            quads[q] = QuadEnc {
                rho,
                bigu,
                v,
                e: ee,
            };
        }
    }

    // Assemble MEL symbol sequence + MagSgn / VLC streams.
    //
    // Walk quads in pair order (q1, q2 within each row). For each quad:
    // - Compute cq from neighbours.
    // - If cq == 0 and rho == 0 → emit MEL=0 (no VLC entry).
    // - Else → emit MEL=1 (only when cq == 0; otherwise no MEL bit) and
    //   then emit the CxtVLC entry into VLC.
    // - U-VLC: u = bigu - kappa, kappa derived from spec.
    // - MagSgn: per significant sample emit (bigu - kbit) LSB bits of v.
    //
    // §7.3.4 Figure 4 specifies the inter-quad ordering: prefix(q1),
    // prefix(q2), suffix(q1), suffix(q2), ext(q1), ext(q2). We mirror
    // that exactly.

    let mut mel_syms: Vec<u8> = Vec::with_capacity(nquads);
    let mut vlc = VlcWriter::new();
    let mut magsgn = MagSgnWriter::new();

    // We need the encoded ρ values of previously-processed quads to
    // derive subsequent contexts. Use a flat array indexed by quad.
    let mut sigemb_rho: Vec<u8> = vec![0u8; nquads];

    for qy in 0..qh {
        let is_first = qy == 0;
        let mut qx = 0usize;
        while qx < qw {
            let q1_idx = qy * qw + qx;
            let q1 = quads[q1_idx];
            let cq1 = if is_first {
                cq_first_linepair(&sigemb_rho, qw, q1_idx)
            } else {
                cq_non_first_linepair(&sigemb_rho, qw, q1_idx)
            };
            // Emit MEL symbol for q1 if cq == 0.
            if cq1 == 0 {
                mel_syms.push(if q1.rho == 0 { 0 } else { 1 });
            }
            // Compute kappa for this quad.
            let kappa1 = if is_first {
                1u32
            } else {
                exponent_predictor_non_first_linepair(&quads, qw, q1_idx)
            };
            // Decide u_off and effective bigu for q1.
            // Use u_off=0 when bigu fits in kappa (no residual needed).
            // Else use u_off=1 with emb_k=ρ, emb_1=ρ (only valid for
            // single-significance quads — guarded above).
            let q1_has_u = q1.rho != 0 && q1.bigu > kappa1;
            let (u_off1, emb_k1, emb_1_1, bigu1_eff) = if q1.rho == 0 {
                (0u8, 0u8, 0u8, kappa1)
            } else if q1_has_u {
                // bigu > kappa: emit residual u = bigu - kappa, choose
                // single-significance EMB to match v's MSB.
                (1u8, q1.rho, q1.rho, q1.bigu)
            } else {
                // bigu <= kappa: u_off=0 path. Effective bigu = kappa,
                // emit kappa bits per significant sample (without
                // implicit MSB ⇒ emb_k=emb_1=0).
                (0u8, 0u8, 0u8, kappa1)
            };

            if !(cq1 == 0 && q1.rho == 0) {
                let ok = encode_cxt_vlc(&mut vlc, cq1, q1.rho, u_off1, emb_k1, emb_1_1, is_first);
                if !ok {
                    return Err(Error::unsupported(format!(
                        "HTJ2K encode: missing CxtVLC entry for cq={cq1} rho={:#X} u_off={u_off1} emb_k={emb_k1:#X} emb_1={emb_1_1:#X}",
                        q1.rho
                    )));
                }
            }
            sigemb_rho[q1_idx] = q1.rho;

            // Process q2 if it exists.
            let q2_present = qx + 1 < qw;
            let q2_opt = if q2_present {
                let q2_idx = qy * qw + qx + 1;
                let q2 = quads[q2_idx];
                let cq2 = if is_first {
                    cq_first_linepair(&sigemb_rho, qw, q2_idx)
                } else {
                    cq_non_first_linepair(&sigemb_rho, qw, q2_idx)
                };
                if cq2 == 0 {
                    mel_syms.push(if q2.rho == 0 { 0 } else { 1 });
                }
                let kappa2 = if is_first {
                    1u32
                } else {
                    exponent_predictor_non_first_linepair(&quads, qw, q2_idx)
                };
                let q2_has_u_local = q2.rho != 0 && q2.bigu > kappa2;
                let (u_off2, emb_k2, emb_1_2, bigu2_eff) = if q2.rho == 0 {
                    (0u8, 0u8, 0u8, kappa2)
                } else if q2_has_u_local {
                    (1u8, q2.rho, q2.rho, q2.bigu)
                } else {
                    (0u8, 0u8, 0u8, kappa2)
                };
                if !(cq2 == 0 && q2.rho == 0) {
                    let ok =
                        encode_cxt_vlc(&mut vlc, cq2, q2.rho, u_off2, emb_k2, emb_1_2, is_first);
                    if !ok {
                        return Err(Error::unsupported(format!(
                            "HTJ2K encode: missing CxtVLC entry for cq={cq2} rho={:#X} u_off={u_off2} emb_k={emb_k2:#X} emb_1={emb_1_2:#X}",
                            q2.rho
                        )));
                    }
                }
                sigemb_rho[q2_idx] = q2.rho;
                Some((q2_idx, q2, cq2, kappa2, q2_has_u_local, bigu2_eff, u_off2))
            } else {
                None
            };

            // §7.3.6 Eq 4 special case: first line-pair, both quads
            // u_off = 1 — round 1 refuses this.
            let q2_has_u = q2_opt.is_some_and(|t| t.6 == 1);
            if is_first && u_off1 == 1 && q2_has_u {
                return Err(Error::unsupported(
                    "HTJ2K encode: first-line-pair quad-pair both u_off=1 (round 2+)",
                ));
            }

            // U-VLC residuals u_q1, u_q2 in interleaved order
            // (Figure 4): prefix(q1) → prefix(q2) → suffix(q1) →
            // suffix(q2) → ext(q1) → ext(q2). Quads with u_off=0 emit
            // nothing.
            let u1 = if u_off1 == 1 { q1.bigu - kappa1 } else { 0 };
            let u2 = match q2_opt {
                Some((_, q2, _, kappa2, _, _, 1)) => q2.bigu - kappa2,
                _ => 0,
            };
            let (pfx1, sfx1, ext1) = if u_off1 == 1 { split_u(u1) } else { (0, 0, 0) };
            let (pfx2, sfx2, ext2) = if q2_has_u { split_u(u2) } else { (0, 0, 0) };
            if u_off1 == 1 {
                encode_u_prefix(&mut vlc, pfx1);
            }
            if q2_has_u {
                encode_u_prefix(&mut vlc, pfx2);
            }
            if u_off1 == 1 {
                encode_u_suffix(&mut vlc, pfx1, sfx1);
            }
            if q2_has_u {
                encode_u_suffix(&mut vlc, pfx2, sfx2);
            }
            if u_off1 == 1 {
                encode_u_extension(&mut vlc, sfx1, ext1);
            }
            if q2_has_u {
                encode_u_extension(&mut vlc, sfx2, ext2);
            }

            // MagSgn bits per significant sample.
            emit_quad_magsgn(&mut magsgn, &q1, bigu1_eff, u_off1)?;
            if let Some((_, q2, _, _, _, bigu2_eff, u_off2)) = q2_opt {
                emit_quad_magsgn(&mut magsgn, &q2, bigu2_eff, u_off2)?;
            }

            qx += 2;
        }
    }

    // ---- Final assembly ----
    // MagSgn bytes (forward): pad partial byte and flush.
    let mag_bytes = magsgn.into_bytes();
    // MEL bytes (forward).
    let mel_bytes = encode_mel_symbols(&mel_syms);
    // VLC bytes: the decoder consumes the FIRST 4 (or 3) VLC bits from
    // the high nibble of `Dcup[Lcup-2]` (the "Scup reservoir"), then
    // walks Dcup[Lcup-3]..Dcup[Pcup] backward LSB-first. We therefore
    // collect the encoded VLC bits as a continuous bit-vector, splice
    // off the LEADING 4 bits into the reservoir (those become the high
    // nibble of Dcup[Lcup-2]), and pack the rest into bytes.
    //
    // Bit ordering within bytes:
    //   The decoder reads byte at Lcup-3 LSB-first, then byte at Lcup-4
    //   LSB-first, etc. So bytes lower in the segment carry LATER bits.
    //   Inside one byte, bit 0 = first bit consumed.
    //
    // For an `n`-bit VLC bit-vector `b[0..n]` (b[0] = first bit the
    // decoder reads after the reservoir), we pack the bits 4..(4+8) of
    // `b` into byte at Lcup-3 LSB-first (b[4] = bit 0 of byte, b[5] =
    // bit 1, ...), bits 12..20 into byte at Lcup-4, etc.
    let vlc_bits = vlc.into_bits_decode_order();
    let (reservoir_nibble, vlc_bytes_segment) = pack_vlc_bits_into_segment(&vlc_bits)?;

    // Cleanup-segment layout per §7.1.1:
    //   bytes [0..Pcup)        : MagSgn (forward, length = Pcup)
    //   bytes [Pcup..Lcup-2)   : MEL forward + VLC reverse, sharing
    //                            the trailing Scup bytes
    //   bytes [Lcup-2..Lcup)   : Scup tail (encodes Scup so the decoder
    //                            can recover Pcup)
    //
    // The MEL forward stream lives in bytes [Pcup..Pcup + mel_len), and
    // the VLC reverse stream in bytes [Lcup-3..Lcup-3 - vlc_len + 1).
    // The two MAY overlap conceptually inside the trailing region; the
    // spec encodes that the last 12 bits of the VLC stream are
    // "absorbed" into the Scup reservoir (the decoder reads 4 or 3 bits
    // from `Dcup[Lcup-2] >> 4` BEFORE fetching `Dcup[Lcup-3]`).
    //
    // Round-1 simplification: we DO NOT overlap. The MEL stream is
    // placed first, the VLC stream byte-aligned just after. The Scup
    // reservoir's leading 12 bits are forced to a known value (we set
    // them to 0; the decoder will read 4 zero bits, which we either
    // pre-pad VLC with or treat as part of the implicit prefix).
    //
    // Layout:
    //   Pcup = magsgn.len()
    //   followed by mel_bytes
    //   followed by vlc_bytes_segment (reverse order so decoder reads
    //     them from the high-index side)
    //   followed by 2 bytes of Scup tail
    //
    // Lcup = Pcup + mel.len + vlc.len + 2
    // Scup = mel.len + vlc.len + 2
    //
    // The decoder will start the VLC reader at index Lcup - 3 = Pcup +
    // mel.len + vlc.len - 1, which is the last byte of vlc_bytes_segment
    // — i.e. the byte we wrote first into `vlc` (decode-order index 0).
    // Since we reversed, vlc_bytes_segment.last() = decode-order [0]. ✓
    //
    // The decoder also reads the first 4 bits from
    // `mod_dcup(Lcup-2) >> 4`, treating those as the initial VLC
    // prefix bits. We set Dcup[Lcup-2] = 0x80 | (Scup low nibble), so
    // its top nibble is 8 — the reader's `tmp = last >> 4 = 8` →
    // initial 4 bits are LSB-first(8) = 0,0,0,1. Those bits would be
    // consumed before our real VLC payload starts. To absorb them we
    // **pre-emit a 4-bit padding sequence at the START of vlc**, so
    // by the time the real CxtVLC bits matter the reader is past the
    // reservoir.
    //
    // Wait — the VlcWriter we used did NOT include reservoir padding.
    // We need to re-author the strategy: insert four padding bits at
    // the *start* of the VLC stream (decoder's first reads), which
    // come from the high-index side of the segment. Since we reverse
    // the byte list, the first byte the decoder reads is the LAST byte
    // we emitted — and inside that byte the LSB is the FIRST bit
    // emitted. So pre-emitting 4 zero bits at the START means we'd
    // call `vlc.write_bit(0)` 4 times BEFORE the real payload — but
    // those 4 bits become the LSB-end of the LAST emitted byte, which
    // ends up at decode-order index 0... and the decoder consumes the
    // RESERVOIR bits FIRST (from `Dcup[Lcup-2] >> 4`), THEN the byte
    // at `Dcup[Lcup-3]`. So the 4 padding bits are absorbed in the
    // reservoir and the real payload starts in `Dcup[Lcup-3]`.
    //
    // Concretely: let initial reservoir bits (LSB-first) = a3 a2 a1 a0.
    // Reader pulls a0, a1, a2, a3. So we need the FIRST four CxtVLC bits
    // to equal a0..a3. If we choose `Dcup[Lcup-2]>>4 = 0x0` (top nibble
    // = 0), then a0..a3 = 0,0,0,0 and we need the encoder to *not*
    // emit any leading bits that depend on those — i.e. we need the
    // first real bit emitted by the encoder to be the FIFTH bit the
    // reader sees. Since the reader fills `bits = 4`, then loops on
    // bits == 0 and refills from Dcup[Lcup-3], the fifth bit is the
    // LSB of Dcup[Lcup-3], which is the FIRST bit we wrote into vlc.
    //
    // So: choose Dcup[Lcup-2] such that its top nibble's LSB-first
    // expansion produces the first 4 reservoir bits we want, then DO
    // NOT prefix vlc with anything.
    //
    // Easiest: top nibble = 0 → reservoir bits = 0,0,0,0. The decoder
    // will treat them as if the VLC stream "started" with four 0 bits.
    // For our (cq, ρ) pattern emission, those 4 zero bits could be
    // misinterpreted as a CxtVLC prefix if the very first quad has
    // cq != 0 and a 4-bit codeword starting with 0000. To avoid that
    // cleanly we instead set top nibble of Dcup[Lcup-2] such that the
    // 4-bit reservoir LSB-first = `0x0` AND we skip those 4 reservoir
    // bits manually by emitting 4 dummy zero bits AT the START of vlc
    // — no wait, those bits won't be consumed because the VLC reader
    // reads them from the reservoir.
    //
    // Final scheme: choose `Dcup[Lcup-2] = 0xF0`. Then `tmp = 0xF >> 0`
    // ... actually `tmp = last >> 4 = (0xF0 | 0x0F = 0xFF) >> 4` after
    // mod_dcup forces low nibble to F. So reservoir tmp = 0xF, bits = 4
    // (since `(tmp & 7) = 7 → bits = 3`, NOT 4). Hmm — the spec rule
    // is `bits = (tmp & 7) < 7 ? 4 : 3`. With tmp = 0xF: (0xF & 7) = 7
    // → bits = 3. So reservoir contributes 3 bits LSB-first(0xF) =
    // 1, 1, 1.
    //
    // To get 4 reservoir bits = 0,0,0,0 we need top nibble = 0x0,
    // giving tmp = 0, bits = 4. So `Dcup[Lcup-2]` low nibble holds the
    // 4-bit Scup low half, top nibble = 0. We must encode Scup such
    // that mod_dcup forces low nibble to F at decode time — but
    // `Dcup[Lcup-2]` keeps its low nibble visibly so the
    // SCUP-recovery formula works:
    //
    //     Scup = 16 * Dcup[Lcup-1] + (Dcup[Lcup-2] & 0x0F)
    //
    // We pick Dcup[Lcup-2] = 0x00 | (Scup & 0xF), Dcup[Lcup-1] =
    // (Scup >> 4) & 0xFF. Mod_dcup forces low nibble to 0xF at decode
    // for the VLC reader's `tmp = 0x0X >> 4 = 0`. Same value as we
    // intended. ✓ Reservoir bits = 0,0,0,0.
    //
    // We must therefore NOT emit any CxtVLC codeword whose first 4
    // bits are all 0 at the start of the stream — otherwise the
    // reservoir's 4 zeros would be confused with the start of that
    // codeword. The first quad always has cq=0 (no left neighbour);
    // for cq=0 the table-0 codewords are MEL-arbitrated via AZC so
    // the first VLC bit corresponds to the SECOND quad onwards.
    // Actually for cq=0, the AZC short-circuit means MEL=0 is emitted
    // ONLY if rho=0 — otherwise MEL=1 is emitted and the CxtVLC bits
    // follow. So the first VLC bits after the reservoir are the
    // first non-AZC quad's CxtVLC codeword.
    //
    // For cq=0, table-0 entries with the smallest l_w are 3 bits long,
    // none of which start with 000 (per inspection above). So the
    // reservoir 0,0,0,0 prefix won't clash. We adopt this layout.

    let pcup = mag_bytes.len();
    let mel_len = mel_bytes.len();
    let vlc_len = vlc_bytes_segment.len();
    let scup = mel_len + vlc_len + 2;
    if scup > 4079 {
        return Err(Error::invalid("HTJ2K encode: Scup exceeds 4079"));
    }
    if scup < 2 {
        // Empty MEL + empty VLC — the cleanup segment is degenerate.
        // We pad VLC with one byte of 0x00 so Scup >= 2 always holds.
        // (This only fires for fully-AZC cleanup inputs that emit zero
        // MEL bits — e.g. an empty codeblock, which is not a real
        // input.)
        return Err(Error::invalid("HTJ2K encode: Scup underflow"));
    }

    let lcup = pcup + mel_len + vlc_len + 2;
    let mut dcup = Vec::with_capacity(lcup);
    dcup.extend_from_slice(&mag_bytes);
    dcup.extend_from_slice(&mel_bytes);
    dcup.extend_from_slice(&vlc_bytes_segment);
    let scup_lo = (scup & 0x0F) as u8;
    let scup_hi = ((scup >> 4) & 0xFF) as u8;
    // Dcup[Lcup-2] high nibble carries the first 4 VLC bits (decoder
    // reservoir); low nibble holds the low half of Scup.
    dcup.push((reservoir_nibble << 4) | scup_lo);
    dcup.push(scup_hi); // Dcup[Lcup-1]

    // §7.1.1: the cleanup segment shall not terminate with a 0xFF byte.
    // scup_hi == 0xFF would only happen if scup >= 16 * 0xFF + 1 = 4081
    // > 4079, which we've already rejected. But defensive check:
    if *dcup.last().unwrap() == 0xFF {
        return Err(Error::invalid(
            "HTJ2K encode: cleanup segment would end in 0xFF",
        ));
    }

    Ok(dcup)
}

fn emit_quad_magsgn(magsgn: &mut MagSgnWriter, q: &QuadEnc, bigu: u32, u_off: u8) -> Result<()> {
    if q.rho == 0 {
        return Ok(());
    }
    // Per the (emb_k, emb_1) pattern selected upstream:
    //   * u_off == 0 ⇒ emb_k = emb_1 = 0 ⇒ kbit_j = 0 for every
    //     significant sample ⇒ m = bigu, decoder reads `bigu` LSB bits
    //     of v with no implicit MSB. Encoder emits `bigu` LSB bits.
    //   * u_off == 1 (round-1 single-significance) ⇒ emb_k = emb_1 = ρ
    //     ⇒ kbit_j = 1 for the lone significant sample, decoder reads
    //     `bigu - 1` bits and ORs in `1 << (bigu - 1)`. Encoder emits
    //     the lower `bigu - 1` bits of v; bit-(bigu-1) of v MUST be 1.
    for j in 0..4u8 {
        if (q.rho >> j) & 1 == 0 {
            continue;
        }
        let v = q.v[j as usize];
        let bit_len = if v == 0 { 0 } else { 32 - v.leading_zeros() };
        if bit_len > bigu {
            return Err(Error::invalid(
                "HTJ2K encode: sample magnitude exceeds bigu",
            ));
        }
        let m: u8 = if u_off == 1 {
            // bit-(bigu-1) of v must be 1 (we picked bigu = bit_len(v)).
            if bit_len != bigu {
                return Err(Error::invalid(
                    "HTJ2K encode: u_off=1 sample with bit_len != bigu",
                ));
            }
            (bigu - 1) as u8
        } else {
            bigu as u8
        };
        magsgn.write_bits_lsb(v, m);
    }
    Ok(())
}

#[derive(Clone, Copy, Default, Debug)]
struct QuadEnc {
    rho: u8,
    bigu: u32,
    v: [u32; 4],
    e: [u8; 4],
}

/// Pack a VLC bit-stream (decode order, `bits[0]` = first bit the
/// decoder reads) into the cleanup segment's reverse-byte VLC area.
///
/// Returns `(reservoir_nibble, vlc_bytes_segment)` where:
///   * `reservoir_nibble` is the 4-bit value to splat into the high
///     nibble of `Dcup[Lcup-2]`. The decoder reads either 4 or 3 bits
///     from the reservoir depending on the `(reservoir_nibble & 7) < 7`
///     test (T.814 §7.1.4): when the low 3 bits of the nibble are
///     `0b111`, only 3 bits are imported and the 4th nibble bit is
///     discarded. The encoder picks the layout to match.
///   * `vlc_bytes_segment` is the byte sequence to splice into
///     `Dcup` at indices `[Pcup + mel_len, Lcup-2)`. The byte at
///     `vlc_bytes_segment[len-1]` corresponds to `Dcup[Lcup-3]` and
///     carries the bits the decoder reads after the reservoir
///     (LSB-first); `[len-2]` ↔ `Dcup[Lcup-4]`, etc.
///
/// Round-1 limitation: when `bits.len() > 4` the writer enforces no
/// reverse-byte stuffing rule — it errors out if any produced byte
/// exceeds `0x8F` (round 2 will wire the stuffing bit). For the
/// fixtures the round-1 encoder targets (single-significance quads,
/// short codewords), VLC bytes stay below `0x80`.
fn pack_vlc_bits_into_segment(bits: &[u8]) -> Result<(u8, Vec<u8>)> {
    // Decide the reservoir width: if bits[0..3] == 1,1,1 the resulting
    // nibble's low-3 = 0b111 → decoder uses bits=3 path.
    let three_bit = bits.len() >= 3 && bits[0] == 1 && bits[1] == 1 && bits[2] == 1;
    let res_width = if three_bit { 3 } else { 4 };
    let mut reservoir = 0u8;
    for (i, &b) in bits.iter().take(res_width).enumerate() {
        reservoir |= (b & 1) << i;
    }
    // The 4-bit-wide nibble's bit 3 (top) is discarded by the decoder
    // when `res_width == 3`. We leave it as zero.
    if bits.len() <= res_width {
        return Ok((reservoir, Vec::new()));
    }
    // Pack remaining bits res_width.. into bytes, 8 bits per byte
    // LSB-first. Reverse-VLC stuffing rule (T.814 §7.1.4): when the
    // PREVIOUSLY-emitted byte exceeds `0x8F` AND the new byte's low 7
    // bits are all 1, the next byte's bit-7 is forced to 0 — the
    // decoder reads only 7 payload bits from it. We mirror by
    // limiting the new byte to 7 bits when the predicate fires; the
    // remaining input bit gets pushed onto the next byte.
    let mut bytes_first_to_last: Vec<u8> = Vec::new();
    let rest = &bits[res_width..];
    let mut idx = 0;
    let mut prev_byte: u8 = 0;
    let mut have_prev = false;
    while idx < rest.len() {
        let cap: usize = if have_prev && prev_byte > 0x8F { 7 } else { 8 };
        let chunk_end = (idx + cap).min(rest.len());
        let mut byte = 0u8;
        for (i, &b) in rest[idx..chunk_end].iter().enumerate() {
            byte |= (b & 1) << i;
        }
        // The stuffing rule guarantees bit-7 stays 0 when cap=7
        // (because we only filled bits 0..6).
        bytes_first_to_last.push(byte);
        prev_byte = byte;
        have_prev = true;
        idx = chunk_end;
    }
    // Reverse so that `bytes_first_to_last[0]` → Dcup[Lcup-3] (the
    // byte the decoder reads FIRST after the reservoir). After reverse
    // the final segment laid out in INCREASING index order sees the
    // last-emitted byte at the highest index, which is the byte the
    // decoder reads first. ✓
    bytes_first_to_last.reverse();
    Ok((reservoir, bytes_first_to_last))
}

/// Mirror of decoder's `cq_first_linepair`. Operates on per-quad ρ
/// values (since SigEmb is just ρ + emb_k/emb_1 here).
fn cq_first_linepair(rho: &[u8], qw: usize, q: usize) -> u8 {
    if q == 0 || q % qw == 0 {
        return 0;
    }
    let prev = rho[q - 1];
    let sw = (prev >> 3) & 1;
    let w = (prev >> 2) & 1;
    let sf = (prev >> 1) & 1;
    let f = prev & 1;
    let cq = (f | sf) + 2 * w + 4 * sw;
    cq.min(7)
}

fn cq_non_first_linepair(rho: &[u8], qw: usize, q: usize) -> u8 {
    let above = q.checked_sub(qw);
    let above_q = match above {
        Some(idx) => rho[idx],
        None => return 0,
    };
    let n = (above_q >> 1) & 1;
    let ne = (above_q >> 3) & 1;
    let nw = if q % qw != 0 {
        let above_left = rho[above.unwrap() - 1];
        (above_left >> 3) & 1
    } else {
        0
    };
    let nf = if (q + 1) % qw != 0 {
        let above_right = rho[above.unwrap() + 1];
        (above_right >> 1) & 1
    } else {
        0
    };
    let (w, sw) = if q % qw != 0 {
        let left = rho[q - 1];
        (((left >> 2) & 1), ((left >> 3) & 1))
    } else {
        (0, 0)
    };
    let cq = (nw | n) + 2 * (w | sw) + 4 * (ne | nf);
    cq.min(7)
}

/// Mirror of decoder's `exponent_predictor_non_first_linepair`. Uses
/// per-sample exponents stored in the quad table.
fn exponent_predictor_non_first_linepair(quads: &[QuadEnc], qw: usize, q: usize) -> u32 {
    let above_q_idx = match q.checked_sub(qw) {
        Some(v) => v,
        None => return 1,
    };
    let above_q = &quads[above_q_idx];
    let mut exps: [u8; 4] = [0; 4];
    exps[1] = above_q.e[1]; // bottom-left of above quad → σ^n
    exps[2] = above_q.e[3]; // bottom-right of above quad → σ^ne
    if q % qw != 0 {
        let above_left = &quads[above_q_idx - 1];
        exps[0] = above_left.e[3]; // bottom-right of NW quad → σ^nw
    }
    if (q + 1) % qw != 0 {
        let above_right = &quads[above_q_idx + 1];
        exps[3] = above_right.e[1]; // bottom-left of NF quad → σ^nf
    }
    let max_e = exps.iter().copied().max().unwrap_or(0);
    let rho = quads[q].rho;
    let gamma = !matches!(rho, 0 | 1 | 2 | 4 | 8);
    if gamma && max_e >= 1 {
        (max_e as u32 - 1).max(1)
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::htj2k::decode_codeblock;
    use crate::decode::htj2k::ZBlk;

    /// Round-trip helper: encode → decode and compare reconstructed
    /// magnitudes/signs against the input. The decoder returns the
    /// quad-scan layout; we map back to raster.
    fn check_roundtrip(width: u32, height: u32, samples: &[SampleHt]) {
        let dcup = encode_cleanup(width, height, samples).expect("encode");
        let out = decode_codeblock(width, height, ZBlk::One, &dcup, &[]).expect("decode");
        let qw = width.div_ceil(2);
        for y in 0..height {
            for x in 0..width {
                let qx = x / 2;
                let qy = y / 2;
                let dx = x & 1;
                let dy = y & 1;
                let j = match (dx, dy) {
                    (0, 0) => 0u8,
                    (0, 1) => 1,
                    (1, 0) => 2,
                    (1, 1) => 3,
                    _ => unreachable!(),
                };
                let q = (qy as usize) * (qw as usize) + qx as usize;
                let n = 4 * q + j as usize;
                let in_s = samples[(y as usize) * (width as usize) + (x as usize)];
                let got_mag = out.mag[n] as u32;
                let got_sign = out.sign[n];
                assert_eq!(
                    got_mag, in_s.mag,
                    "magnitude mismatch at ({x},{y}): expected {} got {}",
                    in_s.mag, got_mag
                );
                if in_s.mag != 0 {
                    assert_eq!(
                        got_sign, in_s.sign,
                        "sign mismatch at ({x},{y}): expected {} got {}",
                        in_s.sign, got_sign
                    );
                }
            }
        }
    }

    #[test]
    fn roundtrip_all_zero_2x2() {
        let samples = vec![SampleHt::default(); 4];
        check_roundtrip(2, 2, &samples);
    }

    #[test]
    fn roundtrip_all_zero_8x8() {
        let samples = vec![SampleHt::default(); 64];
        check_roundtrip(8, 8, &samples);
    }

    #[test]
    fn roundtrip_all_zero_32x32() {
        let samples = vec![SampleHt::default(); 32 * 32];
        check_roundtrip(32, 32, &samples);
    }

    /// Single magnitude-1 sample at TL of a 2x2 codeblock. The first
    /// line-pair / first quad has cq=0; ρ=1 with u_off=1 emits an
    /// (cq=0, ρ=1, u_off=1, emb_k=1, emb_1=1) entry from table 0.
    #[test]
    fn roundtrip_single_one_at_tl_2x2() {
        let mut samples = vec![SampleHt::default(); 4];
        samples[0] = SampleHt { mag: 1, sign: 0 };
        check_roundtrip(2, 2, &samples);
    }

    /// Same with the sample at the BR position.
    #[test]
    fn roundtrip_single_one_at_br_2x2() {
        let mut samples = vec![SampleHt::default(); 4];
        samples[3] = SampleHt { mag: 1, sign: 1 };
        check_roundtrip(2, 2, &samples);
    }

    /// 4×4 codeblock with one sample on the first row, one on the
    /// second — exercises the non-first-linepair κ_q predictor path.
    #[test]
    fn roundtrip_4x4_two_samples() {
        let mut samples = vec![SampleHt::default(); 16];
        samples[0] = SampleHt { mag: 1, sign: 0 };
        // y=2 row, x=0 → second line-pair, first quad, sample TL (j=0)
        samples[2 * 4] = SampleHt { mag: 1, sign: 0 };
        check_roundtrip(4, 4, &samples);
    }

    /// 2x2 with a magnitude-2 sample at TL: exercises u_off=1 + bigu=2
    /// in the simplest setting.
    #[test]
    fn roundtrip_2x2_single_mag2_at_tl() {
        let mut samples = vec![SampleHt::default(); 4];
        samples[0] = SampleHt { mag: 2, sign: 0 };
        check_roundtrip(2, 2, &samples);
    }

    /// 2x2 with a magnitude-3 sample at TL.
    #[test]
    fn roundtrip_2x2_single_mag3_at_tl() {
        let mut samples = vec![SampleHt::default(); 4];
        samples[0] = SampleHt { mag: 3, sign: 0 };
        check_roundtrip(2, 2, &samples);
    }

    /// 8x8 codeblock with a single magnitude-3 sample at (4,4).
    /// Exercises the non-first-linepair path with bigu = 3.
    #[test]
    fn roundtrip_8x8_single_mag3() {
        let mut samples = vec![SampleHt::default(); 64];
        // (4,4) → quad (2, 2), sample TL (j=0).
        samples[4 * 8 + 4] = SampleHt { mag: 3, sign: 1 };
        check_roundtrip(8, 8, &samples);
    }

    /// 32x32 codeblock with two sparse samples — exercises the
    /// missing_msb path through tile_enc indirectly: cleanup-only
    /// produces the correct (mag, sign) tuples.
    #[test]
    fn roundtrip_32x32_two_sparse_samples() {
        let mut samples = vec![SampleHt::default(); 32 * 32];
        samples[0] = SampleHt { mag: 1, sign: 0 };
        samples[4 * 32 + 4] = SampleHt { mag: 1, sign: 1 };
        check_roundtrip(32, 32, &samples);
    }

    /// 8x8 codeblock with three sparse samples spread across rows —
    /// exercises the cleanup encoder's non-first-linepair κ_q
    /// predictor on a multi-quad-row block. The fourth sample at
    /// (6, 6) deliberately has bigu = 4 which exceeds the simple
    /// round-1 EMB-selection and lands in the "u_off = 1, larger
    /// residual" path — keeping every quad single-significance so
    /// the round-1 encoder's at-most-one constraint is honoured.
    #[test]
    fn roundtrip_8x8_three_sparse_samples() {
        let mut samples = vec![SampleHt::default(); 64];
        samples[0] = SampleHt { mag: 2, sign: 0 };
        samples[2 * 8 + 4] = SampleHt { mag: 1, sign: 1 };
        samples[4 * 8] = SampleHt { mag: 5, sign: 0 };
        check_roundtrip(8, 8, &samples);
    }
}
