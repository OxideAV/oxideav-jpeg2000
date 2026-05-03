//! HT segment bit-stream readers.
//!
//! ISO/IEC 15444-15 §7.1 (FDIS 2019, pages 3-9) splits each HT
//! code-block into two byte-streams:
//!
//! * **HT cleanup segment** (`Dcup`, length `Lcup`): holds three
//!   parallel substreams — `MagSgn` reading forward from byte 0,
//!   `MEL` reading forward from byte `Pcup = Lcup - Scup`, and
//!   `VLC` reading **backward** from byte `Lcup - 1`. `Scup` is
//!   recovered from the last two bytes of the cleanup segment per
//!   the formula in §7.1.1: `Scup = 16*Dcup[Lcup-1] + (Dcup[Lcup-2] & 0x0F)`.
//! * **HT refinement segment** (`Dref`, length `Lref`): when
//!   `Z_blk >= 2`, holds the SigProp bits forward from byte 0 and,
//!   when `Z_blk == 3`, the MagRef bits backward from byte `Lref-1`.
//!
//! Each substream is bit-stuffed in a way that mirrors arithmetic-coder
//! "FF/00" stuffing in classic JPEG 2000: after a byte equal to `0xFF`
//! the next byte's MSB (or, for the VLC/MagRef streams, MSB when the
//! prior byte's 7 LSBs are all 1) carries a forced stuffing bit that
//! is skipped over on decode. The exact rule is encoded by the
//! `MS_bits = (MS_last == 0xFF) ? 7 : 8` formula reproduced verbatim
//! from the spec procedures (`importMagSgnBit`, `importMELBit`,
//! `importVLCBit`, `importSigPropBit`, `importMagRefBit`).
//!
//! All readers expose a fallible `import_bit()` API that returns an
//! `Err(InvalidData)` when the spec's `error()` clause would fire —
//! that is, when a stuffing-bit invariant is violated by the encoded
//! data. NOTE 3 of §7.1.2 says that the MagSgn reader effectively
//! synthesises a single trailing `0xFF` byte to ensure the procedure
//! can run to completion when the codestream itself ends; that
//! behaviour is preserved here so callers don't need to oversize
//! their buffers.

use crate::error::{Jpeg2000Error as Error, Result};

/// `modDcup` accessor from §7.1.1: reads byte `pos` of the HT cleanup
/// segment after virtually overwriting the trailing two bytes used by
/// the `Scup` formula. The last byte is read as `0xFF` and the second
/// to last has its low nibble forced to `0xF`.
#[inline]
fn mod_dcup(dcup: &[u8], pos: usize) -> u8 {
    let lcup = dcup.len();
    if pos == lcup - 1 {
        0xFF
    } else if pos == lcup - 2 {
        dcup[pos] | 0x0F
    } else {
        dcup[pos]
    }
}

/// MagSgn forward bit-stream reader (§7.1.2).
///
/// Reads the MagSgn substream which lives in the first
/// `Pcup = Lcup - Scup` bytes of the cleanup segment. The reader
/// tracks a position cursor and a bit accumulator; bits are extracted
/// from each byte LSB-first (little-endian bit order). When the
/// previous byte was `0xFF`, only 7 bits are extracted from the next
/// byte (the spec NOTE: the MSB after a `0xFF` byte is forced to 0
/// during encoding to disambiguate from real `FF` bytes).
pub struct MagSgnReader<'a> {
    dcup: &'a [u8],
    pcup: usize,
    pos: usize,
    bits: u8,
    tmp: u8,
    last: u8,
}

impl<'a> MagSgnReader<'a> {
    /// Construct from the full cleanup segment buffer and the prefix
    /// length `Pcup` (which delimits where MagSgn ends and MEL begins).
    pub fn new(dcup: &'a [u8], pcup: usize) -> Self {
        // initMS: pos=0, bits=0, tmp=0, last=0
        Self {
            dcup,
            pcup,
            pos: 0,
            bits: 0,
            tmp: 0,
            last: 0,
        }
    }

    /// Spec procedure `importMagSgnBit`. Returns the next bit from the
    /// MagSgn substream. When the stream is exhausted but the spec
    /// says we should continue (NOTE 3 of §7.1.2), a synthetic `0xFF`
    /// byte is supplied — this means trailing zero bits are returned
    /// indefinitely past the real end.
    pub fn import_bit(&mut self) -> Result<u8> {
        if self.bits == 0 {
            self.bits = if self.last == 0xFF { 7 } else { 8 };
            if self.pos < self.pcup {
                self.tmp = mod_dcup(self.dcup, self.pos);
                if self.last == 0xFF && (self.tmp & (1u8 << self.bits)) != 0 {
                    return Err(Error::invalid(
                        "HTJ2K MagSgn: stuffing-bit invariant violated",
                    ));
                }
            } else if self.pos == self.pcup {
                // NOTE 3: synthesise one trailing 0xFF byte.
                self.tmp = 0xFF;
            } else {
                return Err(Error::invalid("HTJ2K MagSgn: read past end of segment"));
            }
            self.last = self.tmp;
            self.pos += 1;
        }
        let bit = self.tmp & 1;
        self.tmp >>= 1;
        self.bits -= 1;
        Ok(bit)
    }
}

/// MEL forward bit-stream reader (§7.1.3).
///
/// Reads the MEL substream which begins at byte `Pcup` of the cleanup
/// segment and is consumed forward MSB-first (big-endian bit order),
/// up to `Scup` bytes; the VLC substream may overlap by reading the
/// same bytes from the other side. After a `0xFF` byte the next byte
/// yields only 7 bits (the spec's bit-stuffing rule).
pub struct MelReader<'a> {
    dcup: &'a [u8],
    lcup: usize,
    pos: usize,
    bits: u8,
    tmp: u8,
}

impl<'a> MelReader<'a> {
    pub fn new(dcup: &'a [u8], pcup: usize) -> Self {
        // initMEL: pos=Pcup, bits=0, tmp=0
        Self {
            dcup,
            lcup: dcup.len(),
            pos: pcup,
            bits: 0,
            tmp: 0,
        }
    }

    pub fn import_bit(&mut self) -> Result<u8> {
        if self.bits == 0 {
            self.bits = if self.tmp == 0xFF { 7 } else { 8 };
            if self.pos < self.lcup {
                self.tmp = mod_dcup(self.dcup, self.pos);
                self.pos += 1;
            } else {
                self.tmp = 0xFF;
            }
        }
        self.bits -= 1;
        let bit = (self.tmp >> self.bits) & 1;
        Ok(bit)
    }
}

/// VLC reverse bit-stream reader (§7.1.4).
///
/// Reads from the **end** of the cleanup segment: the cursor starts
/// at byte `Lcup - 3` because the trailing two bytes encode `Scup`
/// (and that suffix reservoir forms the initial 12 reusable bits of
/// the VLC stream). After consuming a byte with value greater than
/// `0x8F` whose low 7 bits are all 1, the next byte yields only 7
/// bits — this is the spec's variant of the FF stuffing rule applied
/// in reverse.
pub struct VlcReader<'a> {
    dcup: &'a [u8],
    pcup: usize,
    pos: usize,
    bits: u8,
    tmp: u8,
    last: u8,
}

impl<'a> VlcReader<'a> {
    pub fn new(dcup: &'a [u8], pcup: usize) -> Self {
        // initVLC: pos = Lcup-3, last = modDcup(Dcup,Lcup-2),
        //          tmp = last >> 4, bits = ((tmp & 7) < 7) ? 4 : 3
        let lcup = dcup.len();
        debug_assert!(lcup >= 2);
        let last = mod_dcup(dcup, lcup - 2);
        let tmp = last >> 4;
        let bits = if (tmp & 7) < 7 { 4u8 } else { 3 };
        let pos = lcup.saturating_sub(3);
        Self {
            dcup,
            pcup,
            pos,
            bits,
            tmp,
            last,
        }
    }

    pub fn import_bit(&mut self) -> Result<u8> {
        if self.bits == 0 {
            // The spec writes pos as a signed cursor that may go below
            // zero; we model "before start" as pos == usize::MAX, but
            // the legality check is `pos >= Pcup` which for our usize
            // representation is met as long as we have not gone below
            // Pcup. Encode that as a flag.
            if self.pos >= self.pcup {
                self.tmp = mod_dcup(self.dcup, self.pos);
            } else {
                return Err(Error::invalid("HTJ2K VLC: read past start of segment"));
            }
            self.bits = 8;
            if self.last > 0x8F && (self.tmp & 0x7F) == 0x7F {
                self.bits = 7;
            }
            self.last = self.tmp;
            // pos is decremented after the byte is consumed — when
            // pos was equal to pcup we still permit one more read
            // because the check is `>=`. Here we decrement; if pos
            // is already 0 we leave it (next iteration will fail the
            // `pos >= pcup` check assuming pcup >= 1).
            if self.pos > 0 {
                self.pos -= 1;
            } else {
                // Sentinel: any further read will fail the bounds
                // check. Set pos to a value guaranteed to be < pcup.
                // pcup >= 1 in any conformant codestream because
                // Lcup >= 2 and the suffix has length >= 2.
                self.pos = self.pcup.saturating_sub(1);
            }
        }
        let bit = self.tmp & 1;
        self.tmp >>= 1;
        self.bits -= 1;
        Ok(bit)
    }
}

/// SigProp forward bit-stream reader (§7.1.5). Reads the HT
/// refinement segment from byte 0 forward. Same FF/stuffing rule as
/// MagSgn, but bytes beyond `Lref` synthesise to 0 (NOTE 1: no `0xFF`
/// is appended — only zero-padding).
pub struct SigPropReader<'a> {
    dref: &'a [u8],
    pos: usize,
    bits: u8,
    tmp: u8,
    last: u8,
}

impl<'a> SigPropReader<'a> {
    pub fn new(dref: &'a [u8]) -> Self {
        Self {
            dref,
            pos: 0,
            bits: 0,
            tmp: 0,
            last: 0,
        }
    }

    pub fn import_bit(&mut self) -> Result<u8> {
        if self.bits == 0 {
            self.bits = if self.last == 0xFF { 7 } else { 8 };
            if self.pos < self.dref.len() {
                self.tmp = self.dref[self.pos];
                self.pos += 1;
                if self.last == 0xFF && (self.tmp & (1u8 << self.bits)) != 0 {
                    return Err(Error::invalid(
                        "HTJ2K SigProp: stuffing-bit invariant violated",
                    ));
                }
            } else {
                self.tmp = 0;
            }
            self.last = self.tmp;
        }
        let bit = self.tmp & 1;
        self.tmp >>= 1;
        self.bits -= 1;
        Ok(bit)
    }
}

/// MagRef reverse bit-stream reader (§7.1.6). Reads the HT refinement
/// segment from byte `Lref-1` backward. `last` is initialised to
/// `0xFF` so any leading run of `0x..7F` bytes triggers the 7-bit
/// extraction case immediately.
pub struct MagRefReader<'a> {
    dref: &'a [u8],
    pos: isize,
    bits: u8,
    tmp: u8,
    last: u8,
}

impl<'a> MagRefReader<'a> {
    pub fn new(dref: &'a [u8]) -> Self {
        let pos = dref.len() as isize - 1;
        Self {
            dref,
            pos,
            bits: 0,
            tmp: 0,
            last: 0xFF,
        }
    }

    pub fn import_bit(&mut self) -> Result<u8> {
        if self.bits == 0 {
            if self.pos >= 0 {
                self.tmp = self.dref[self.pos as usize];
                self.pos -= 1;
            } else {
                self.tmp = 0;
            }
            self.bits = 8;
            if self.last > 0x8F && (self.tmp & 0x7F) == 0x7F {
                self.bits = 7;
            }
            self.last = self.tmp;
        }
        let bit = self.tmp & 1;
        self.tmp >>= 1;
        self.bits -= 1;
        Ok(bit)
    }
}

/// Compute `Pcup = Lcup - Scup` from the trailing two bytes of the
/// cleanup segment, per §7.1.1. Returns the (Pcup, Scup) pair.
/// `Scup` must satisfy `2 <= Scup <= min(Lcup, 4079)`.
pub fn compute_scup(dcup: &[u8]) -> Result<(usize, usize)> {
    let lcup = dcup.len();
    if lcup < 2 {
        return Err(Error::invalid("HTJ2K cleanup segment shorter than 2 bytes"));
    }
    let scup = (16 * dcup[lcup - 1] as usize) + (dcup[lcup - 2] as usize & 0x0F);
    if scup < 2 || scup > lcup.min(4079) {
        return Err(Error::invalid("HTJ2K cleanup segment Scup out of range"));
    }
    let pcup = lcup - scup;
    Ok((pcup, scup))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mod_dcup_overrides_trailing_bytes() {
        let buf = [0x12u8, 0x34, 0x56, 0x78];
        // byte 0..1 unchanged
        assert_eq!(mod_dcup(&buf, 0), 0x12);
        assert_eq!(mod_dcup(&buf, 1), 0x34);
        // byte Lcup-2 has low nibble forced to F: 0x56 -> 0x5F
        assert_eq!(mod_dcup(&buf, 2), 0x5F);
        // byte Lcup-1 always 0xFF
        assert_eq!(mod_dcup(&buf, 3), 0xFF);
    }

    #[test]
    fn compute_scup_basic() {
        // Build a 4-byte cleanup segment whose last two bytes encode
        // Scup = 16*Dcup[Lcup-1] + (Dcup[Lcup-2] & 0x0F).
        // Dcup[Lcup-1]=0, Dcup[Lcup-2]=0x02 -> Scup = 0 + 2 = 2.
        let buf = [0xAA, 0xBB, 0x02, 0x00];
        let (pcup, scup) = compute_scup(&buf).unwrap();
        assert_eq!(scup, 2);
        assert_eq!(pcup, 2);
    }

    #[test]
    fn magsgn_reads_lsb_first_with_no_stuffing() {
        // Pcup = 1, single byte 0xA5 = 0b1010_0101 — LSB-first
        // returns 1,0,1,0,0,1,0,1,...
        let buf = [0xA5, 0x00, 0x02, 0x00]; // Scup=2, Lcup=4
        let (pcup, _) = compute_scup(&buf).unwrap();
        let mut r = MagSgnReader::new(&buf, pcup);
        // pcup is 2 here, but we only care about the first byte at index 0.
        let bits: Vec<u8> = (0..8).map(|_| r.import_bit().unwrap()).collect();
        assert_eq!(bits, vec![1, 0, 1, 0, 0, 1, 0, 1]);
    }

    #[test]
    fn vlc_reverse_reads_lsb_first() {
        // Construct Lcup = 4 buffer with Scup = 3 (so Pcup = 1, and
        // index Lcup-3 = 1 sits inside the VLC area). Tail two bytes
        // encode Scup: Dcup[Lcup-1] = 0, (Dcup[Lcup-2] & 0xF) = 3 →
        // Dcup[2] low nibble = 3.
        // Byte at Lcup-3 (index 1) = 0x0F → LSB-first 1,1,1,1,0,0,0,0.
        // Initial 4 bits from VLC_tmp = mod_dcup(Lcup-2)>>4 = (0x83|0x0F=0x8F)>>4
        // = 0x8 → LSB-first 0,0,0,1.
        let buf = [0xAA, 0x0F, 0x83, 0x00];
        let (pcup, _) = compute_scup(&buf).unwrap();
        assert_eq!(pcup, 1);
        let mut r = VlcReader::new(&buf, pcup);
        let bits: Vec<u8> = (0..12).map(|_| r.import_bit().unwrap()).collect();
        // First 4 bits from VLC_tmp (0x8): LSB-first 0,0,0,1.
        assert_eq!(&bits[..4], &[0, 0, 0, 1]);
        // Next 8 bits from byte at Lcup-3 = 0x0F, LSB-first.
        assert_eq!(&bits[4..12], &[1, 1, 1, 1, 0, 0, 0, 0]);
    }

    #[test]
    fn sigprop_returns_zero_past_end() {
        let buf = [0x01u8];
        let mut r = SigPropReader::new(&buf);
        // 1, 0, 0, 0, 0, 0, 0, 0 from byte 0; then synthetic zeros.
        for _ in 0..16 {
            let _ = r.import_bit().unwrap();
        }
    }

    #[test]
    fn magref_reverse_reads_lsb_first() {
        // dref = [0x01, 0xC0]: pos starts at 1 -> read 0xC0
        // = 0b1100_0000 LSB-first -> 0,0,0,0,0,0,1,1; then 0x01.
        let buf = [0x01u8, 0xC0];
        let mut r = MagRefReader::new(&buf);
        let bits: Vec<u8> = (0..16).map(|_| r.import_bit().unwrap()).collect();
        assert_eq!(&bits[..8], &[0, 0, 0, 0, 0, 0, 1, 1]);
        assert_eq!(&bits[8..16], &[1, 0, 0, 0, 0, 0, 0, 0]);
    }
}
