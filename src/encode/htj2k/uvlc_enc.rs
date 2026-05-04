//! U-VLC encoder (inverse of [`crate::decode::htj2k::uvlc`]).
//!
//! Per T.814 §7.3.6 / Table 3:
//!
//! ```text
//!   u = u_pfx + u_sfx + 4 * u_ext
//! ```
//!
//! where the prefix is 1/2/3/3 bits (codeword `1`, `01`, `001`, `000`
//! mapping to `1`, `2`, `3`, `5`), the suffix is 0, 1, or 5 bits
//! depending on the prefix value, and the extension is 0 or 4 bits
//! depending on the suffix.
//!
//! All bits are written LSB-first into the [`super::streams_enc::VlcWriter`].

use super::streams_enc::VlcWriter;

/// Encode the prefix portion. `u_pfx ∈ {1, 2, 3, 5}` per Table 3.
/// Returns the prefix value (so the caller can pick the suffix width).
pub fn encode_u_prefix(w: &mut VlcWriter, u_pfx: u8) {
    match u_pfx {
        1 => {
            // codeword `1` (single-bit, b0 = 1)
            w.write_bit(1);
        }
        2 => {
            // codeword `01` LSB-first: b0=0, b1=1
            w.write_bit(0);
            w.write_bit(1);
        }
        3 => {
            // codeword `001` LSB-first: b0=0, b1=0, b2=1
            w.write_bit(0);
            w.write_bit(0);
            w.write_bit(1);
        }
        5 => {
            // codeword `000` LSB-first: b0=0, b1=0, b2=0
            w.write_bit(0);
            w.write_bit(0);
            w.write_bit(0);
        }
        _ => panic!("invalid u_pfx {u_pfx}: must be 1, 2, 3, or 5"),
    }
}

/// Suffix width for a given prefix value (Table 3): 0 bits when
/// `u_pfx < 3`, 1 bit when `u_pfx == 3`, 5 bits when `u_pfx == 5`.
pub fn suffix_width(u_pfx: u8) -> u8 {
    if u_pfx < 3 {
        0
    } else if u_pfx == 3 {
        1
    } else {
        5
    }
}

/// Encode the suffix portion (at most 5 bits LSB-first).
pub fn encode_u_suffix(w: &mut VlcWriter, u_pfx: u8, u_sfx: u8) {
    let n = suffix_width(u_pfx);
    if n == 0 {
        return;
    }
    // u_sfx must fit in `n` bits (5-bit range = 0..=31).
    debug_assert!(u_sfx < (1u8 << n) || n == 5 && u_sfx <= 31);
    for i in 0..n {
        let b = (u_sfx >> i) & 1;
        w.write_bit(b);
    }
}

/// Extension width per Table 3: 4 bits when `u_sfx >= 28`, otherwise 0.
pub fn extension_width(u_sfx: u8) -> u8 {
    if u_sfx >= 28 {
        4
    } else {
        0
    }
}

/// Encode the extension portion (at most 4 bits LSB-first).
pub fn encode_u_extension(w: &mut VlcWriter, u_sfx: u8, u_ext: u8) {
    let n = extension_width(u_sfx);
    if n == 0 {
        return;
    }
    debug_assert!(u_ext < (1u8 << n));
    for i in 0..n {
        let b = (u_ext >> i) & 1;
        w.write_bit(b);
    }
}

/// Pick `(u_pfx, u_sfx, u_ext)` so that `u_pfx + u_sfx + 4 * u_ext == u`.
///
/// We minimise the prefix length (favour `u_pfx = 1` when `u == 1`,
/// `u_pfx = 2` when `u == 2`, `u_pfx = 3` when `u ∈ [3, 5]`,
/// `u_pfx = 5` when `u >= 5`). When the residual after the prefix
/// would not fit in the suffix's bit width, we fall back to the
/// next-larger prefix or use the extension.
pub fn split_u(u: u32) -> (u8, u8, u8) {
    // `u` should fit in the encoder's working range. The cleanup
    // encoder bounds U_q to 32 (each band has at most ~16 bit-planes
    // in the FBCOT magnitude representation), so `u = U - kappa` is
    // small.
    if u == 1 {
        return (1, 0, 0);
    }
    if u == 2 {
        return (2, 0, 0);
    }
    // Prefer prefix=3 (1-bit suffix) when u ∈ {3, 4}.
    if u == 3 {
        return (3, 0, 0);
    }
    if u == 4 {
        return (3, 1, 0);
    }
    // For u >= 5, use prefix = 5 with a 5-bit suffix and possible
    // 4-bit extension (extension width fires when sfx >= 28).
    // u = 5 + sfx + 4 * ext, with sfx in [0..=31], ext in [0..=15] when
    // sfx >= 28.
    let r = u - 5;
    if r <= 27 {
        return (5, r as u8, 0);
    }
    // r >= 28: sfx ∈ [28..=31] selects the extension path. Suffix bits
    // 0..3 of `(r mod 32)`? Actually decoder: `u_sfx + 4 * u_ext`
    // accumulates over (5-bit suffix LSB-first then 4-bit ext LSB-first
    // when suffix >= 28). So the relation `u = 5 + u_sfx + 4 * u_ext`
    // with `u_sfx` in `[28, 31]` and `u_ext` in `[0, 15]` covers
    // `r ∈ [28, 31 + 4 * 15] = [28, 91]`.
    if r > 91 {
        panic!("u={u} (r={r}) exceeds U-VLC encoder range (max 96)");
    }
    // Pick the smallest u_sfx >= 28 such that r - u_sfx is a non-
    // negative multiple of 4 within [0, 60].
    for sfx in 28..=31u32 {
        if r >= sfx {
            let diff = r - sfx;
            if diff % 4 == 0 && diff / 4 <= 15 {
                return (5, sfx as u8, (diff / 4) as u8);
            }
        }
    }
    // Final fallback (should not happen given the bounds above):
    // use sfx = 28 and the closest extension that overshoots, but this
    // is a programmer error — assert for safety.
    panic!("u={u} could not be split into (pfx=5, sfx, ext)");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::htj2k::streams::{compute_scup, VlcReader};
    use crate::decode::htj2k::uvlc::{decode_u_extension, decode_u_prefix, decode_u_suffix};

    /// Round-trip helper: emit a single residual `u` via the encoder,
    /// build a tiny Dcup, and decode it back. Asserts the recovered
    /// triple matches the encoded one.
    fn roundtrip(u: u32) {
        let (pfx, sfx, ext) = split_u(u);
        let mut w = VlcWriter::new();
        encode_u_prefix(&mut w, pfx);
        encode_u_suffix(&mut w, pfx, sfx);
        encode_u_extension(&mut w, sfx, ext);
        let bits = w.into_bits_decode_order();
        // Splice into a Dcup with the leading 4 bits in the Scup
        // reservoir (high nibble of Dcup[Lcup-2]) and the rest in a
        // single byte at index Lcup-3 LSB-first.
        let mut bytes_segment = Vec::new();
        if bits.len() <= 4 {
            // All bits fit in the reservoir; no extra byte needed.
        } else {
            let mut byte = 0u8;
            for (i, &b) in bits[4..].iter().enumerate() {
                if i >= 8 {
                    bytes_segment.push(byte);
                    byte = 0;
                }
                let pos = i % 8;
                byte |= b << pos;
            }
            // Push final partial byte if any bits past index 4.
            if bits.len() > 4 {
                bytes_segment.push(byte);
            }
            // Reverse for VLC decode order: byte at Lcup-3 holds
            // bits 4..12, byte at Lcup-4 holds bits 12..20, etc.
            // Our packing put bits 4..12 in bytes_segment[0] which
            // corresponds to the LARGEST index (Lcup-3) — already in
            // the right "decode-first" order, but the segment layout
            // requires DESCENDING index for descending bit position
            // (= ascending segment-start position). The reader reads
            // Dcup[Lcup-3] first; we placed bits 4..12 there. Then
            // Dcup[Lcup-4] which holds bits 12..20. So our bytes go
            // in reverse order at the END of the segment.
            bytes_segment.reverse();
        }
        let reservoir_nibble: u8 = {
            let mut n = 0u8;
            for (i, &b) in bits.iter().take(4).enumerate() {
                n |= b << i;
            }
            n
        };
        let mut dcup = vec![0u8]; // dummy MagSgn byte
        dcup.extend_from_slice(&bytes_segment);
        let scup = bytes_segment.len() + 2;
        // Dcup[Lcup-2]: high nibble = reservoir, low nibble = scup low.
        dcup.push((reservoir_nibble << 4) | (scup & 0x0F) as u8);
        dcup.push(((scup >> 4) & 0xFF) as u8);

        let (pcup, _scup) = compute_scup(&dcup).unwrap();
        let mut r = VlcReader::new(&dcup, pcup);
        let dpfx = decode_u_prefix(&mut r).unwrap();
        let dsfx = decode_u_suffix(&mut r, dpfx).unwrap();
        let dext = decode_u_extension(&mut r, dsfx).unwrap();
        let recovered = dpfx as u32 + dsfx as u32 + 4 * dext as u32;
        assert_eq!(
            recovered, u,
            "u={u}: pfx={pfx} sfx={sfx} ext={ext} recovered as pfx={dpfx} sfx={dsfx} ext={dext}"
        );
    }

    #[test]
    fn uvlc_roundtrip_small_values() {
        for u in 1..=10u32 {
            roundtrip(u);
        }
    }

    #[test]
    fn uvlc_roundtrip_medium_values() {
        for u in [11u32, 16, 20, 27, 28, 32, 50, 64, 91].iter() {
            roundtrip(*u);
        }
    }

    #[test]
    fn split_u_obeys_sum_relation() {
        for u in 1..=80u32 {
            let (pfx, sfx, ext) = split_u(u);
            assert_eq!(
                pfx as u32 + sfx as u32 + 4 * ext as u32,
                u,
                "u={u}: pfx={pfx} sfx={sfx} ext={ext}"
            );
        }
    }
}
