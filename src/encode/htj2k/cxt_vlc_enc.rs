//! CxtVLC encoder (inverse of [`crate::decode::htj2k::cxt_vlc`]).
//!
//! Encoder side of T.814 §7.3.5. Given a decoded `(ρ_q, u_off, ε^k, ε^1)`
//! tuple for a quad and the precomputed context value `cq`, locate the
//! row of the appropriate Annex C table (table 0 for first line-pair,
//! table 1 otherwise) and emit the codeword (length `l_w`, integer `w`)
//! LSB-first into the [`super::streams_enc::VlcWriter`].
//!
//! Tables are shared with the decoder via
//! [`crate::decode::htj2k::cxt_vlc_tables`]; since they are dual-use
//! reference data (the spec defines a single Annex C table per
//! line-pair flavour) using them here is not a "decoder-internal"
//! cross-reference — the tables are the authoritative spec data both
//! sides consume.

use super::streams_enc::VlcWriter;
use crate::decode::htj2k::cxt_vlc_tables::{Entry, CXT_VLC_TABLE_0, CXT_VLC_TABLE_1};

/// Look up the codeword `(w, l_w)` for the given context + tuple.
/// Returns `None` if no exact-match entry exists in the table — the
/// caller will treat this as an encoder limitation (the round-1
/// encoder restricts itself to the AZC short-circuit + the lowest-
/// magnitude rows that are guaranteed to be present in the tables).
fn find_entry(table: &[Entry], cq: u8, rho: u8, u_off: u8, emb_k: u8, emb_1: u8) -> Option<Entry> {
    table
        .iter()
        .copied()
        .find(|e| e.0 == cq && e.1 == rho && e.2 == u_off && e.3 == emb_k && e.4 == emb_1)
}

/// Emit the codeword for `(ρ, u_off, ε^k, ε^1)` at context `cq`.
///
/// `is_first_linepair` selects table 0 vs table 1.
pub fn encode_cxt_vlc(
    w: &mut VlcWriter,
    cq: u8,
    rho: u8,
    u_off: u8,
    emb_k: u8,
    emb_1: u8,
    is_first_linepair: bool,
) -> bool {
    let table: &[Entry] = if is_first_linepair {
        CXT_VLC_TABLE_0
    } else {
        CXT_VLC_TABLE_1
    };
    let Some(entry) = find_entry(table, cq, rho, u_off, emb_k, emb_1) else {
        return false;
    };
    let cwd = entry.5 as u32;
    let len = entry.6;
    // Write `len` bits LSB-first.
    for i in 0..len {
        let b = ((cwd >> i) & 1) as u8;
        w.write_bit(b);
    }
    true
}

/// Pick `(ρ, emb_k, emb_1)` for an HT codeblock encoder when only the
/// per-sample magnitudes are known (no entropy-coding side info from
/// the spec encoder). For round 1 we use the simplest scheme:
///
/// * `ρ` = 4-bit OR of the four samples' significance bits (this is
///   the spec definition).
/// * `emb_k` = ρ AND the per-sample mask of "this sample's
///   magnitude is exactly U_q" (i.e. its leading bit is at the
///   exponent boundary).
/// * `emb_1` = ρ AND the per-sample mask of "this sample's
///   magnitude has bit (U_q - 1) set" — for the round-1 encoder, since
///   each sample's magnitude `μ ∈ {0, 1, ...}` represented in MagSgn as
///   `v = 2(μ-1) + s` (T.814 §7.3.8) we always choose `U_q` such that
///   `μ_max ∈ [2^(U-1), 2^U - 1]`, so the topmost bit is always set
///   for the dominant sample. We mark `emb_k` for that sample so the
///   decoder consumes (U-1) MagSgn bits for it; `emb_1` matches the
///   dominant sample's parity bit.
///
/// This is one of many valid encoder choices; the spec leaves
/// considerable freedom in how `(emb_k, emb_1)` are selected as long as
/// the resulting codeword exists in the Annex C table.
pub fn pick_emb_simple(rho: u8, mu: [u32; 4], bigu: u32) -> (u8, u8) {
    if bigu == 0 {
        return (0, 0);
    }
    let mut k_mask = 0u8;
    let mut one_mask = 0u8;
    for j in 0..4u8 {
        if (rho >> j) & 1 == 0 {
            continue;
        }
        let m = mu[j as usize];
        if m == 0 {
            continue;
        }
        // Recover v from μ per T.814 §7.3.8: v = 2(μ-1) + s with
        // s = 0 for the EMB selection (sign is carried inside MagSgn,
        // so we pick the "even" v = 2(μ-1)). exponent-bit mask: bit-
        // position of v's MSB.
        let v = 2 * (m - 1);
        let bit_len = 32 - v.leading_zeros();
        // ε^k = 1 means decoder will subtract 1 from the m-bit count
        // (so it imports `bigu - 1` MagSgn bits and combines with the
        // ε^1 bit). We set ε^k for this sample whenever the magnitude
        // ALREADY hits the band's exponent bound — i.e. when bit_len
        // == bigu. That tells the decoder: "this sample's MSB equals
        // bit U-1 by construction; you don't need to read it."
        let kbit = if bit_len == bigu { 1 } else { 0 };
        // ε^1 = 1 means bit U is set (the implicit bit added back at
        // position `m` per `val |= (ibit << m)`). For our encoding
        // choice we set ε^1 = ε^k since the topmost bit of v is exactly
        // at position U-1 when `bit_len == U` (the implicit MSB is
        // shifted in at position m = U - kbit = U - 1).
        let onebit = kbit;
        if kbit != 0 {
            k_mask |= 1u8 << j;
        }
        if onebit != 0 {
            one_mask |= 1u8 << j;
        }
    }
    (k_mask, one_mask)
}

/// Search Annex C for a usable row encoding the given `(cq, rho)` quad
/// at u_off=1. Round 2 uses this to handle multi-significance quads:
/// for each candidate row we test whether the per-sample (kbit, ibit)
/// constraints are satisfied by the supplied `v[4]` values at the
/// chosen `bigu`. Returns the matching `(emb_k, emb_1, codeword_len)`
/// of the shortest valid row, or `None` if no row in the table
/// satisfies the constraints.
///
/// Constraint per T.814 §7.3.8: for each significant sample j the
/// decoder computes `m = bigu - kbit_j` then reads m LSB bits of v
/// and ORs `ibit_j << m`. So the encoder requires
/// `bit(m, v_j) == ibit_j` where `m = bigu - kbit_j`. When `kbit_j = 0`
/// we have `m = bigu` and (since v_j fits in `bigu` bits) `bit(bigu,
/// v_j) = 0`, so `ibit_j` must be 0. When `kbit_j = 1` we have
/// `m = bigu - 1` and `ibit_j` must equal `bit(bigu-1, v_j)`.
pub fn pick_emb_for_uoff1(
    cq: u8,
    rho: u8,
    v: [u32; 4],
    bigu: u32,
    is_first: bool,
) -> Option<(u8, u8, u8)> {
    let table: &[Entry] = if is_first {
        CXT_VLC_TABLE_0
    } else {
        CXT_VLC_TABLE_1
    };
    let mut best: Option<(u8, u8, u8)> = None;
    for &(e_cq, e_rho, e_uoff, e_k, e_1, _w, l_w) in table {
        if e_cq != cq || e_rho != rho || e_uoff != 1 {
            continue;
        }
        // Verify per-sample constraints.
        let mut ok = true;
        for j in 0..4u8 {
            if (rho >> j) & 1 == 0 {
                // Non-significant sample: emb_k / emb_1 bits MUST be 0
                // (the table itself maintains this invariant; defensive
                // check).
                if (e_k >> j) & 1 != 0 || (e_1 >> j) & 1 != 0 {
                    ok = false;
                    break;
                }
                continue;
            }
            let kbit = (e_k >> j) & 1;
            let ibit = (e_1 >> j) & 1;
            let m = if kbit == 1 {
                bigu.saturating_sub(1)
            } else {
                bigu
            };
            // bit(m, v_j) must equal ibit. Note bigu may be 0 only when
            // rho is 0 (handled by caller), so m >= 0 always when rho's
            // j-th bit is set.
            let v_bit = if m >= 32 { 0 } else { (v[j as usize] >> m) & 1 } as u8;
            if v_bit != ibit {
                ok = false;
                break;
            }
            // Also the v_j magnitude must fit: bit_len(v_j) <= bigu.
            let bit_len = if v[j as usize] == 0 {
                0
            } else {
                32 - v[j as usize].leading_zeros()
            };
            if bit_len > bigu {
                ok = false;
                break;
            }
            // When kbit=1 the implicit MSB at bigu-1 must actually
            // carry the value — we already checked bit(bigu-1, v_j) ==
            // ibit. But when ibit=1 we further need bit_len == bigu
            // (otherwise ibit=1 says the high bit is 1 but the value
            // fits in fewer bits). The `v_bit == ibit` check above
            // already captured that since bit(bigu-1) is 1 iff
            // bit_len == bigu.
        }
        if !ok {
            continue;
        }
        match best {
            None => best = Some((e_k, e_1, l_w)),
            Some((_, _, prev_len)) if l_w < prev_len => best = Some((e_k, e_1, l_w)),
            _ => {}
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::htj2k::cxt_vlc_tables::{CXT_VLC_TABLE_0, CXT_VLC_TABLE_1};

    /// Every entry of table 0 must round-trip: emit its codeword via
    /// the encoder and verify the bit pattern matches the table's
    /// `w / l_w` fields.
    #[test]
    fn table0_codewords_match_after_encode() {
        for &(cq, rho, u_off, emb_k, emb_1, w_expected, len_expected) in CXT_VLC_TABLE_0 {
            let mut wr = VlcWriter::new();
            assert!(encode_cxt_vlc(&mut wr, cq, rho, u_off, emb_k, emb_1, true));
            let bits = wr.into_bits_decode_order();
            assert_eq!(bits.len(), len_expected as usize);
            // Reassemble first `len_expected` bits LSB-first.
            let mut assembled: u32 = 0;
            for (i, &b) in bits.iter().enumerate() {
                assembled |= (b as u32) << i;
            }
            assert_eq!(
                assembled, w_expected as u32,
                "table0 cq={cq} rho={rho:X} u_off={u_off} k={emb_k:X} 1={emb_1:X}: got {assembled:#X}, want {w_expected:#X}",
            );
        }
    }

    /// Same for table 1 (non-first-linepair).
    #[test]
    fn table1_codewords_match_after_encode() {
        for &(cq, rho, u_off, emb_k, emb_1, w_expected, len_expected) in CXT_VLC_TABLE_1 {
            let mut wr = VlcWriter::new();
            assert!(encode_cxt_vlc(&mut wr, cq, rho, u_off, emb_k, emb_1, false));
            let bits = wr.into_bits_decode_order();
            assert_eq!(bits.len(), len_expected as usize);
            let mut assembled: u32 = 0;
            for (i, &b) in bits.iter().enumerate() {
                assembled |= (b as u32) << i;
            }
            assert_eq!(
                assembled, w_expected as u32,
                "table1 cq={cq} rho={rho:X} u_off={u_off} k={emb_k:X} 1={emb_1:X}: got {assembled:#X}, want {w_expected:#X}",
            );
        }
    }
}
