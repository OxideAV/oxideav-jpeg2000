//! Context-adaptive VLC decoding (§7.3.5 of ISO/IEC 15444-15:2019,
//! FDIS pages 14-16) on top of the Annex C tables.
//!
//! The decoder reads bits LSB-first from the [`super::streams::VlcReader`]
//! until the (codeword, length) pair matches an entry of the chosen
//! Annex C table whose `c_q` field equals the precomputed context.
//! When matched, the entry yields the four output values
//! `(ρ_q, u_q^off, ε^k_q, ε^1_q)` that the cleanup pass needs.
//!
//! The Annex C tables enumerate every (c_q, codeword) pair the
//! encoder may emit; lookup never needs to backtrack. Maximum codeword
//! length is 7 bits (per Table 3 + Annex C).

use super::cxt_vlc_tables::{Entry, CXT_VLC_TABLE_0, CXT_VLC_TABLE_1};
use super::mel::MelDecoder;
use super::streams::{MelReader, VlcReader};
use oxideav_core::{Error, Result};

/// Tuple returned by `decodeCxtVLC` / `decodeSigEMB`:
/// `(ρ_q, u_q^off, ε^k_q, ε^1_q)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SigEmb {
    /// `ρ_q` — 4-bit significance pattern of the quad's four samples.
    pub rho: u8,
    /// `u_q^off` — 1 if the U-VLC residual offset bit is set.
    pub u_off: u8,
    /// `ε^k_q` — 4-bit "EMB-known" mask.
    pub emb_k: u8,
    /// `ε^1_q` — 4-bit "EMB-1" mask.
    pub emb_1: u8,
}

impl SigEmb {
    pub const ZERO: Self = Self {
        rho: 0,
        u_off: 0,
        emb_k: 0,
        emb_1: 0,
    };
}

/// `decodeCxtVLC` (§7.3.5). Imports up to 7 bits LSB-first from the
/// VLC stream and matches them against the appropriate Annex C table
/// (table 0 for the first line-pair quads where `q < QW`, table 1
/// otherwise) for the supplied context value.
pub fn decode_cxt_vlc(
    reader: &mut VlcReader<'_>,
    cq: u8,
    is_first_linepair: bool,
) -> Result<SigEmb> {
    let table: &[Entry] = if is_first_linepair {
        CXT_VLC_TABLE_0
    } else {
        CXT_VLC_TABLE_1
    };

    let mut cwd: u32 = 0;
    let mut len: u8 = 0;
    loop {
        let bit = reader.import_bit()? as u32;
        cwd |= bit << len;
        len += 1;
        if let Some(e) = table_match(table, cq, cwd, len) {
            return Ok(SigEmb {
                rho: e.1,
                u_off: e.2,
                emb_k: e.3,
                emb_1: e.4,
            });
        }
        if len > 7 {
            return Err(Error::invalid(
                "HTJ2K CxtVLC: codeword length exceeded 7 bits without match",
            ));
        }
    }
}

fn table_match(table: &[Entry], cq: u8, cwd: u32, len: u8) -> Option<Entry> {
    let mask: u32 = if len >= 32 {
        u32::MAX
    } else {
        (1u32 << len) - 1
    };
    let cwd_lo = cwd & mask;
    for e in table {
        if e.0 == cq && e.6 == len && (e.5 as u32) == cwd_lo {
            return Some(*e);
        }
    }
    None
}

/// `decodeSigEMB` (§7.3.5). Combines MEL-decoded AZC short-circuit
/// with the CxtVLC fallback path.
pub fn decode_sig_emb(
    vlc: &mut VlcReader<'_>,
    mel: &mut MelReader<'_>,
    mel_decoder: &mut MelDecoder,
    cq: u8,
    is_first_linepair: bool,
) -> Result<SigEmb> {
    if cq == 0 {
        let sym = mel_decoder.decode_sym(mel)?;
        if sym == 0 {
            return Ok(SigEmb::ZERO);
        }
    }
    decode_cxt_vlc(vlc, cq, is_first_linepair)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Within the same context `c_q` and length `l_w`, no two
    /// entries may share a codeword. (The spec table tolerates
    /// "longer entries whose first `l_w` bits coincide with a
    /// shorter codeword"; the greedy length-by-length matcher in
    /// [`decode_cxt_vlc`] reaches the shorter entry first, so the
    /// longer one is unused. The constraint we actually need is
    /// uniqueness within a (cq, len) bucket.)
    #[test]
    fn table0_unique_per_context_and_length() {
        check_unique_per_len(CXT_VLC_TABLE_0, "table_0");
    }

    #[test]
    fn table1_unique_per_context_and_length() {
        check_unique_per_len(CXT_VLC_TABLE_1, "table_1");
    }

    fn check_unique_per_len(table: &[Entry], name: &str) {
        for cq in 0..8u8 {
            for len in 1..=7u8 {
                let entries: Vec<&Entry> =
                    table.iter().filter(|e| e.0 == cq && e.6 == len).collect();
                for (i, a) in entries.iter().enumerate() {
                    for b in entries.iter().skip(i + 1) {
                        if a.5 == b.5 {
                            panic!(
                                "{}: cq={:X} len={}: duplicate cwd 0x{:02X}",
                                name, cq, len, a.5
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn table0_has_at_least_one_entry_per_context() {
        for cq in 0..8u8 {
            let n = CXT_VLC_TABLE_0.iter().filter(|e| e.0 == cq).count();
            assert!(n > 0, "table_0 missing entries for cq={}", cq);
        }
    }

    #[test]
    fn table1_has_at_least_one_entry_per_context() {
        for cq in 0..8u8 {
            let n = CXT_VLC_TABLE_1.iter().filter(|e| e.0 == cq).count();
            assert!(n > 0, "table_1 missing entries for cq={}", cq);
        }
    }
}
