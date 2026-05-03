//! U-VLC variable-length code for unsigned residuals (§7.3.6 +
//! Table 3 of ISO/IEC 15444-15:2019, FDIS pages 16-18).
//!
//! The U-VLC coding is decomposed into three import steps:
//!
//! * `decodeUPrefix` — at most 3 bits, returns one of {1, 2, 3, 5}.
//! * `decodeUSuffix` — 0, 1, or 5 bits, depending on the prefix.
//! * `decodeUExtension` — 0 or 4 bits, depending on the suffix.
//!
//! The reconstructed residual is `u = u_pfx + u_sfx + 4 * u_ext`,
//! per Formula (3) of §7.3.6.

use super::streams::VlcReader;
use crate::error::Result;

/// `decodeUPrefix` — Table 3 column "Prefix" decoded from the VLC
/// stream LSB-first.
pub fn decode_u_prefix(reader: &mut VlcReader<'_>) -> Result<u8> {
    let b0 = reader.import_bit()?;
    if b0 == 1 {
        return Ok(1);
    }
    let b1 = reader.import_bit()?;
    if b1 == 1 {
        return Ok(2);
    }
    let b2 = reader.import_bit()?;
    Ok(if b2 == 1 { 3 } else { 5 })
}

/// `decodeUSuffix` — given a previously-decoded prefix, import 0, 1
/// or 5 bits and return the suffix value.
pub fn decode_u_suffix(reader: &mut VlcReader<'_>, u_pfx: u8) -> Result<u8> {
    if u_pfx < 3 {
        return Ok(0);
    }
    let mut val = reader.import_bit()?;
    if u_pfx == 3 {
        return Ok(val);
    }
    for i in 1..5 {
        let b = reader.import_bit()?;
        val += b << i;
    }
    Ok(val)
}

/// `decodeUExtension` — when the suffix is 28 or higher, import 4
/// extension bits LSB-first (with bit 0 implicitly 1 per Table 3).
pub fn decode_u_extension(reader: &mut VlcReader<'_>, u_sfx: u8) -> Result<u8> {
    if u_sfx < 28 {
        return Ok(0);
    }
    let mut val = reader.import_bit()?;
    for i in 1..4 {
        let b = reader.import_bit()?;
        val += b << i;
    }
    Ok(val)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::htj2k::streams::compute_scup;

    fn vlc_buf_from_byte(b: u8) -> Vec<u8> {
        // VLC reads from byte Lcup-3 backward. Build a 4-byte buffer
        // where index Lcup-3 == 1 holds `b`; the trailing two bytes
        // encode Scup = 3 (so Pcup = 1, and index 1 sits inside the
        // VLC area).
        vec![0xAA, b, 0x83, 0x00]
    }

    #[test]
    fn u_prefix_codes() {
        // Codeword "1" (LSB-first) -> u_pfx = 1.
        // First byte of cleanup segment is at index 0 (Lcup-3 when Lcup=3).
        let buf = vlc_buf_from_byte(0x01);
        let (pcup, _) = compute_scup(&buf).unwrap();
        let mut r = VlcReader::new(&buf, pcup);
        // Reader yields the 4-bit suffix (0) first, then byte at Lcup-3.
        // Skip the first 4 bits.
        for _ in 0..4 {
            r.import_bit().unwrap();
        }
        assert_eq!(decode_u_prefix(&mut r).unwrap(), 1);

        // Codeword "01" -> u_pfx = 2 (bits 1=0, 2=1 LSB-first → byte 0x02).
        let buf = vlc_buf_from_byte(0x02);
        let (pcup, _) = compute_scup(&buf).unwrap();
        let mut r = VlcReader::new(&buf, pcup);
        for _ in 0..4 {
            r.import_bit().unwrap();
        }
        assert_eq!(decode_u_prefix(&mut r).unwrap(), 2);

        // Codeword "001" (bit0=0,bit1=0,bit2=1) → u_pfx = 3 → byte 0x04.
        let buf = vlc_buf_from_byte(0x04);
        let (pcup, _) = compute_scup(&buf).unwrap();
        let mut r = VlcReader::new(&buf, pcup);
        for _ in 0..4 {
            r.import_bit().unwrap();
        }
        assert_eq!(decode_u_prefix(&mut r).unwrap(), 3);

        // Codeword "000" → u_pfx = 5 → byte 0x00.
        let buf = vlc_buf_from_byte(0x00);
        let (pcup, _) = compute_scup(&buf).unwrap();
        let mut r = VlcReader::new(&buf, pcup);
        for _ in 0..4 {
            r.import_bit().unwrap();
        }
        assert_eq!(decode_u_prefix(&mut r).unwrap(), 5);
    }
}
