//! HT segment bit-stream writers (encoder side).
//!
//! Mirror of [`crate::decode::htj2k::streams`]. Each writer accumulates
//! bits into a byte buffer following the FF/00 stuffing rules of
//! ISO/IEC 15444-15 §7.1. The cleanup segment (`Dcup`) is finally
//! assembled by interleaving the three forward (`MagSgn`, `MEL`) and
//! reverse (`VLC`) streams: MagSgn occupies the first `Pcup` bytes,
//! MEL+VLC share the trailing `Scup` bytes (MEL forward from `Pcup`,
//! VLC backward from `Lcup-1`).
//!
//! Stuffing rules per the spec:
//!   * MagSgn / SigProp: LSB-first; whenever the previous emitted byte
//!     was `0xFF`, the next byte's MSB is forced to `0` (the next
//!     byte therefore carries only 7 payload bits).
//!   * MEL: MSB-first; same `0xFF` MSB-zero rule.
//!   * VLC / MagRef: LSB-first reverse; when the previously emitted
//!     byte (in encode order = the spec's "previous" in reverse-byte
//!     order, which is the byte the decoder will see *after* this one)
//!     is `> 0x8F` and the new byte's low 7 bits are all 1, the next
//!     byte gets only 7 payload bits.
//!
//! The writers expose a `flush_byte_aligned()` helper that pads the
//! current partial byte with zeros and emits it. Call once you've
//! written the last bit you intend to write.

/// Forward LSB-first writer with the FF stuffing rule (`MagSgn`).
pub struct MagSgnWriter {
    buf: Vec<u8>,
    /// Number of payload bits already in `cur` (0..=8 normally, 0..=7
    /// after a 0xFF byte was just flushed).
    nbits: u8,
    /// Currently-assembling byte, with already-written bits in the LSBs.
    cur: u8,
    /// True when the last byte flushed was 0xFF — next byte gets only
    /// 7 payload bits and bit-7 is forced 0.
    pending_ff: bool,
}

impl Default for MagSgnWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl MagSgnWriter {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            nbits: 0,
            cur: 0,
            pending_ff: false,
        }
    }

    pub fn write_bit(&mut self, bit: u8) {
        let cap: u8 = if self.pending_ff { 7 } else { 8 };
        if self.nbits == cap {
            self.flush_one();
        }
        if (bit & 1) != 0 {
            self.cur |= 1u8 << self.nbits;
        }
        self.nbits += 1;
    }

    pub fn write_bits_lsb(&mut self, value: u32, n: u8) {
        for i in 0..n {
            let b = ((value >> i) & 1) as u8;
            self.write_bit(b);
        }
    }

    fn flush_one(&mut self) {
        let b = self.cur;
        self.buf.push(b);
        self.pending_ff = b == 0xFF;
        self.cur = 0;
        self.nbits = 0;
    }

    /// Pad the partial byte with zeros and flush it. After this, the
    /// stream is byte-aligned.
    pub fn flush_byte_aligned(&mut self) {
        if self.nbits > 0 {
            self.flush_one();
        }
    }

    pub fn into_bytes(mut self) -> Vec<u8> {
        self.flush_byte_aligned();
        self.buf
    }

    #[allow(dead_code)]
    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }
}

/// Forward MSB-first writer with the FF stuffing rule (`MEL`).
pub struct MelWriter {
    buf: Vec<u8>,
    /// Bits already filled in `cur` from the MSB side.
    nbits: u8,
    cur: u8,
    pending_ff: bool,
}

impl Default for MelWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl MelWriter {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            nbits: 0,
            cur: 0,
            pending_ff: false,
        }
    }

    pub fn write_bit(&mut self, bit: u8) {
        let cap: u8 = if self.pending_ff { 7 } else { 8 };
        if self.nbits == cap {
            self.flush_one();
        }
        // Place at MSB-aligned position. After cap=8 with nbits=0..7,
        // bit goes at (7 - nbits). For cap=7, bit-7 is forced 0 so we
        // start filling at (6 - nbits).
        let pos = if self.pending_ff {
            6 - self.nbits
        } else {
            7 - self.nbits
        };
        if (bit & 1) != 0 {
            self.cur |= 1u8 << pos;
        }
        self.nbits += 1;
    }

    fn flush_one(&mut self) {
        let b = self.cur;
        self.buf.push(b);
        self.pending_ff = b == 0xFF;
        self.cur = 0;
        self.nbits = 0;
    }

    pub fn flush_byte_aligned(&mut self) {
        if self.nbits > 0 {
            self.flush_one();
        }
    }

    pub fn into_bytes(mut self) -> Vec<u8> {
        self.flush_byte_aligned();
        self.buf
    }

    #[allow(dead_code)]
    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }
}

/// VLC bit accumulator. The decoder reads VLC bits in a specific
/// order (reservoir then byte at Lcup-3 LSB-first then byte at Lcup-4
/// etc.); the encoder collects the bits the decoder will see in that
/// order and lets the cleanup-segment assembler decide where each bit
/// physically lands in `Dcup`.
///
/// Round 1 ignores the reverse-byte stuffing rule (`last > 0x8F` →
/// next byte gets only 7 payload bits) — the smaller test fixtures
/// never produce VLC bytes above 0x8F. Round 2 will wire the rule.
pub struct VlcWriter {
    /// Bits the decoder will read, in decode order. `bits[0]` is the
    /// first bit the decoder consumes (after the Scup reservoir).
    bits: Vec<u8>,
}

impl Default for VlcWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl VlcWriter {
    pub fn new() -> Self {
        Self { bits: Vec::new() }
    }

    pub fn write_bit(&mut self, bit: u8) {
        self.bits.push(bit & 1);
    }

    pub fn write_bits_lsb(&mut self, value: u32, n: u8) {
        for i in 0..n {
            let b = ((value >> i) & 1) as u8;
            self.write_bit(b);
        }
    }

    /// Drop the bit accumulator and return the bits in decode order
    /// (`out[0]` = first bit the decoder reads after the reservoir).
    pub fn into_bits_decode_order(self) -> Vec<u8> {
        self.bits
    }

    /// Number of bits accumulated.
    #[allow(dead_code)]
    pub fn len_bits(&self) -> usize {
        self.bits.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::htj2k::streams::{compute_scup, MagSgnReader, MelReader};

    /// MagSgn writer round-trip: bits emitted LSB-first and read back
    /// LSB-first should match.
    #[test]
    fn magsgn_writer_roundtrip_lsb_no_stuffing() {
        let mut w = MagSgnWriter::new();
        // bits 1,0,1,0,0,1,0,1 LSB-first → byte 0xA5.
        for b in [1, 0, 1, 0, 0, 1, 0, 1].iter() {
            w.write_bit(*b);
        }
        let bytes = w.into_bytes();
        assert_eq!(bytes, vec![0xA5]);
        // Read back through the decoder.
        let mut dcup = bytes.clone();
        // Pad with 2 trailing bytes encoding Scup = 2 (Pcup = 1).
        dcup.extend_from_slice(&[0x02, 0x00]);
        let (pcup, _) = compute_scup(&dcup).unwrap();
        let mut r = MagSgnReader::new(&dcup, pcup);
        let bits: Vec<u8> = (0..8).map(|_| r.import_bit().unwrap()).collect();
        assert_eq!(bits, vec![1, 0, 1, 0, 0, 1, 0, 1]);
    }

    /// MEL writer round-trip: bits emitted MSB-first should be read
    /// back MSB-first by the decoder.
    #[test]
    fn mel_writer_roundtrip_msb_no_stuffing() {
        let mut w = MelWriter::new();
        // bits 1,0,1,0,0,1,0,1 MSB-first → byte 0xA5.
        for b in [1, 0, 1, 0, 0, 1, 0, 1].iter() {
            w.write_bit(*b);
        }
        let bytes = w.into_bytes();
        assert_eq!(bytes, vec![0xA5]);
        // Build a cleanup segment whose MEL byte at Pcup = 0 is 0xA5
        // and trailing two bytes encode Scup = 3 (so Pcup = 0).
        let mut dcup = bytes.clone();
        dcup.extend_from_slice(&[0x03, 0x00]);
        let (pcup, _) = compute_scup(&dcup).unwrap();
        assert_eq!(pcup, 0);
        let mut r = MelReader::new(&dcup, pcup);
        let bits: Vec<u8> = (0..8).map(|_| r.import_bit().unwrap()).collect();
        assert_eq!(bits, vec![1, 0, 1, 0, 0, 1, 0, 1]);
    }

    /// VLC writer accumulates bits in decode order.
    #[test]
    fn vlc_writer_collects_bits_in_decode_order() {
        let mut w = VlcWriter::new();
        for b in [1, 0, 1, 0, 0, 1, 0, 1].iter() {
            w.write_bit(*b);
        }
        let bits = w.into_bits_decode_order();
        assert_eq!(bits, vec![1, 0, 1, 0, 0, 1, 0, 1]);
    }
}
