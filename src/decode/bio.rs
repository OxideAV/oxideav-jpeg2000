//! Bit I/O reader for tier-2 packet headers.
//!
//! JPEG 2000 packet headers (ISO/IEC 15444-1 §B.9, §B.10) are packed
//! MSB-first bit strings. A 0xFF byte introduces the stuff-bit rule: if
//! the previous byte output was 0xFF, only 7 payload bits follow in the
//! next byte (the MSB must be 0). The reader mirrors this with an
//! `ff_pending` flag.

/// Sequential bit reader used to parse packet headers.
pub struct Bio<'a> {
    buf: &'a [u8],
    /// Index of the next byte to load into `buf`.
    pos: usize,
    /// Pending bits in `byte` (0..=8).
    ct: u32,
    /// Buffered byte, MSB-aligned so `ct` bits remain in the high end.
    byte: u32,
    /// True if the previous byte we consumed was 0xFF, meaning the next
    /// byte carries only 7 usable bits.
    ff_pending: bool,
}

impl<'a> Bio<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Bio {
            buf,
            pos: 0,
            ct: 0,
            byte: 0,
            ff_pending: false,
        }
    }

    /// Read a single bit.
    #[inline]
    pub fn read_bit(&mut self) -> u32 {
        if self.ct == 0 {
            self.refill();
        }
        self.ct -= 1;
        (self.byte >> self.ct) & 1
    }

    /// Read `n` bits MSB-first. `n` ≤ 32.
    pub fn read(&mut self, n: u32) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.read_bit();
        }
        v
    }

    /// Align the reader to the next byte boundary, matching the JPEG 2000
    /// "inalign" rule at the end of a packet header. Discards any
    /// remaining unread bits in the current byte.
    pub fn inalign(&mut self) {
        if self.ff_pending {
            // The next byte must be a complement of the 0xFF — i.e.
            // carry only 7 payload bits. After alignment, we simply
            // drop the current byte and let the next `refill` observe
            // the 0xFF rule.
            self.ct = 0;
            self.ff_pending = false;
        } else {
            self.ct = 0;
        }
    }

    /// Number of bytes the reader has consumed so far, counting the
    /// current in-progress byte as consumed once any bit has been read.
    pub fn numbytes_read(&self) -> usize {
        self.pos
    }

    fn refill(&mut self) {
        let b = if self.pos < self.buf.len() {
            let v = self.buf[self.pos];
            self.pos += 1;
            v
        } else {
            // Past the declared packet-header buffer — pad with zeros.
            0
        };
        if self.ff_pending {
            self.byte = (b & 0x7F) as u32;
            self.ct = 7;
        } else {
            self.byte = b as u32;
            self.ct = 8;
        }
        self.ff_pending = b == 0xFF;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bits_msb_first() {
        let data = [0b1010_0110u8];
        let mut bio = Bio::new(&data);
        assert_eq!(bio.read(4), 0b1010);
        assert_eq!(bio.read(4), 0b0110);
    }

    #[test]
    fn honours_ff_stuff_bit() {
        // 0xFF then 0b0110_1010 → the high bit of the second byte must
        // be skipped, leaving 7 usable bits (0110_1010 → 0x6A but only
        // bits[6..0] = 0b110_1010).
        let data = [0xFF, 0x6A];
        let mut bio = Bio::new(&data);
        assert_eq!(bio.read(8), 0xFF);
        // Only 7 payload bits in the next byte, MSB stripped.
        assert_eq!(bio.read(7), 0b110_1010);
    }
}
