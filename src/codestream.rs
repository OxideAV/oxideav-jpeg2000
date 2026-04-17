//! JPEG 2000 Part-1 codestream (J2K) marker parser.
//!
//! Parses the high-level structure of a raw `.j2k` codestream (ISO/IEC
//! 15444-1, Annex A). This is enough to recover image geometry and per-
//! component bit-depths without touching the actual compressed sample
//! data — the wavelet transform, MQ arithmetic coder, and EBCOT tier-1
//! / tier-2 decoding are still to come.
//!
//! Markers handled:
//!
//! | Marker | Hex    | Meaning                           | State captured |
//! |--------|--------|-----------------------------------|----------------|
//! | SOC    | FF 4F  | Start of codestream               | presence       |
//! | SIZ    | FF 51  | Image + tile sizes                | geometry + component list |
//! | COD    | FF 52  | Coding style (default)            | raw segment    |
//! | QCD    | FF 5C  | Quantisation (default)            | raw segment    |
//! | COC    | FF 53  | Coding style (per-component)      | raw segment    |
//! | QCC    | FF 5D  | Quantisation (per-component)      | raw segment    |
//! | RGN    | FF 5E  | Region of interest                | raw segment    |
//! | POC    | FF 5F  | Progression order change          | raw segment    |
//! | PPM    | FF 60  | Packed packet headers, main       | raw segment    |
//! | TLM    | FF 55  | Tile-part lengths                 | raw segment    |
//! | PLM    | FF 57  | Packet lengths, main              | raw segment    |
//! | CRG    | FF 63  | Component registration            | raw segment    |
//! | COM    | FF 64  | Comment                           | raw segment    |
//! | SOT    | FF 90  | Start of tile-part                | tile index, length |
//! | SOD    | FF 93  | Start of data (no length field)   | offset/length of compressed body |
//! | EOC    | FF D9  | End of codestream                 | presence       |
//!
//! All marker segments except SOC / SOD / EOC carry a big-endian `Lseg`
//! length that includes its own two bytes. SOD has no length — the
//! compressed data runs to the next SOT or EOC.

use oxideav_core::{Error, Result};

/// Two-byte JPEG 2000 marker code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marker(pub u16);

impl Marker {
    pub const SOC: Marker = Marker(0xFF4F);
    pub const SIZ: Marker = Marker(0xFF51);
    pub const COD: Marker = Marker(0xFF52);
    pub const COC: Marker = Marker(0xFF53);
    pub const TLM: Marker = Marker(0xFF55);
    pub const PLM: Marker = Marker(0xFF57);
    pub const PLT: Marker = Marker(0xFF58);
    pub const QCD: Marker = Marker(0xFF5C);
    pub const QCC: Marker = Marker(0xFF5D);
    pub const RGN: Marker = Marker(0xFF5E);
    pub const POC: Marker = Marker(0xFF5F);
    pub const PPM: Marker = Marker(0xFF60);
    pub const PPT: Marker = Marker(0xFF61);
    pub const CRG: Marker = Marker(0xFF63);
    pub const COM: Marker = Marker(0xFF64);
    pub const SOT: Marker = Marker(0xFF90);
    pub const SOP: Marker = Marker(0xFF91);
    pub const EPH: Marker = Marker(0xFF92);
    pub const SOD: Marker = Marker(0xFF93);
    pub const EOC: Marker = Marker(0xFFD9);
}

/// Per-component description from the SIZ segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentInfo {
    /// Bit depth minus one in the low 7 bits; high bit signed.
    pub ssiz: u8,
    /// Horizontal sub-sampling factor (1 = full rate).
    pub xrsiz: u8,
    /// Vertical sub-sampling factor.
    pub yrsiz: u8,
}

impl ComponentInfo {
    /// Bit depth of samples (1..=38 per the spec).
    pub fn bit_depth(&self) -> u32 {
        (self.ssiz as u32 & 0x7F) + 1
    }

    /// Whether the component stores signed values.
    pub fn is_signed(&self) -> bool {
        (self.ssiz & 0x80) != 0
    }
}

/// Parsed SIZ segment — image grid + per-component parameters.
#[derive(Debug, Clone)]
pub struct Siz {
    /// Capabilities word (Rsiz). 0 = unrestricted Part-1.
    pub rsiz: u16,
    /// Image size on the reference grid (Xsiz, Ysiz).
    pub xsiz: u32,
    pub ysiz: u32,
    /// Image origin on the reference grid (XOsiz, YOsiz).
    pub xosiz: u32,
    pub yosiz: u32,
    /// Nominal tile size (XTsiz, YTsiz).
    pub xtsiz: u32,
    pub ytsiz: u32,
    /// Tile grid origin (XTOsiz, YTOsiz).
    pub xtosiz: u32,
    pub ytosiz: u32,
    pub components: Vec<ComponentInfo>,
}

impl Siz {
    /// Width of the actual image canvas.
    pub fn image_width(&self) -> u32 {
        self.xsiz.saturating_sub(self.xosiz)
    }

    /// Height of the actual image canvas.
    pub fn image_height(&self) -> u32 {
        self.ysiz.saturating_sub(self.yosiz)
    }

    /// Number of image components (channels).
    pub fn num_components(&self) -> usize {
        self.components.len()
    }
}

/// Records a single tile-part's position + length inside the codestream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TilePart {
    /// Tile index (Isot).
    pub tile_index: u16,
    /// Tile-part index within that tile (TPsot).
    pub tile_part_index: u8,
    /// Total number of tile-parts for this tile, or 0 if not yet known (TNsot).
    pub tile_part_count: u8,
    /// Tile-part total length in bytes from the SOT marker itself to the
    /// end of compressed data, inclusive (Psot). 0 means "runs to EOC".
    pub psot: u32,
    /// Byte offset of the SOD marker inside the codestream.
    pub sod_offset: usize,
    /// Number of compressed data bytes following SOD for this tile-part.
    pub sod_length: usize,
}

/// Full parse result for one J2K codestream.
#[derive(Debug, Clone)]
pub struct Codestream {
    pub siz: Siz,
    /// Raw COD segment payload (after Lcod). `None` if absent (malformed).
    pub cod: Option<Vec<u8>>,
    /// Raw QCD segment payload (after Lqcd). `None` if absent.
    pub qcd: Option<Vec<u8>>,
    pub tile_parts: Vec<TilePart>,
    /// Byte offset of the EOC marker, or `None` if the stream was truncated.
    pub eoc_offset: Option<usize>,
}

/// Parse a raw `.j2k` codestream.
///
/// Walks the marker chain starting at SOC and returns a [`Codestream`]
/// with image geometry and tile-part positions. Compressed sample data
/// is left in place — callers who want to decode must still feed each
/// tile-part's bytes through a (future) tier-1 / tier-2 pipeline.
pub fn parse(buf: &[u8]) -> Result<Codestream> {
    let mut cur = Cursor::new(buf);

    let m = cur.read_marker()?;
    if m != Marker::SOC {
        return Err(Error::invalid(format!(
            "jpeg2000: expected SOC (FF4F) at offset 0, got {:04X}",
            m.0
        )));
    }

    let m = cur.read_marker()?;
    if m != Marker::SIZ {
        return Err(Error::invalid(format!(
            "jpeg2000: expected SIZ (FF51) after SOC, got {:04X}",
            m.0
        )));
    }
    let siz = parse_siz(&mut cur)?;

    let mut cod: Option<Vec<u8>> = None;
    let mut qcd: Option<Vec<u8>> = None;
    let mut tile_parts: Vec<TilePart> = Vec::new();
    let mut eoc_offset: Option<usize> = None;

    loop {
        if cur.remaining() == 0 {
            break;
        }
        let marker_off = cur.pos();
        let m = cur.read_marker()?;
        match m {
            Marker::COD => {
                let seg = cur.read_len_segment()?;
                cod = Some(seg.to_vec());
            }
            Marker::QCD => {
                let seg = cur.read_len_segment()?;
                qcd = Some(seg.to_vec());
            }
            Marker::COC
            | Marker::QCC
            | Marker::RGN
            | Marker::POC
            | Marker::PPM
            | Marker::PPT
            | Marker::TLM
            | Marker::PLM
            | Marker::PLT
            | Marker::CRG
            | Marker::COM => {
                let _ = cur.read_len_segment()?;
            }
            Marker::SOT => {
                let seg = cur.read_len_segment()?;
                if seg.len() != 8 {
                    return Err(Error::invalid(format!(
                        "jpeg2000: SOT payload must be 8 bytes, got {}",
                        seg.len()
                    )));
                }
                let tile_index = u16::from_be_bytes([seg[0], seg[1]]);
                let psot = u32::from_be_bytes([seg[2], seg[3], seg[4], seg[5]]);
                let tile_part_index = seg[6];
                let tile_part_count = seg[7];

                let sod_m = cur.read_marker()?;
                if sod_m != Marker::SOD {
                    return Err(Error::invalid(format!(
                        "jpeg2000: expected SOD (FF93) after SOT, got {:04X}",
                        sod_m.0
                    )));
                }
                let sod_offset = cur.pos();

                let sod_length = if psot == 0 {
                    scan_to_next_boundary(buf, sod_offset)?
                } else {
                    let tile_part_end = marker_off
                        .checked_add(psot as usize)
                        .ok_or_else(|| Error::invalid("jpeg2000: SOT Psot overflow"))?;
                    if tile_part_end > buf.len() {
                        return Err(Error::invalid(
                            "jpeg2000: SOT Psot points past end of codestream",
                        ));
                    }
                    tile_part_end - sod_offset
                };

                tile_parts.push(TilePart {
                    tile_index,
                    tile_part_index,
                    tile_part_count,
                    psot,
                    sod_offset,
                    sod_length,
                });
                cur.skip(sod_length)?;
            }
            Marker::EOC => {
                eoc_offset = Some(marker_off);
                break;
            }
            Marker::SOC => {
                return Err(Error::invalid(
                    "jpeg2000: unexpected second SOC inside codestream",
                ));
            }
            other => {
                return Err(Error::invalid(format!(
                    "jpeg2000: unknown marker {:04X} at offset {}",
                    other.0, marker_off
                )));
            }
        }
    }

    Ok(Codestream {
        siz,
        cod,
        qcd,
        tile_parts,
        eoc_offset,
    })
}

fn parse_siz(cur: &mut Cursor<'_>) -> Result<Siz> {
    let seg = cur.read_len_segment()?;
    if seg.len() < 36 {
        return Err(Error::invalid(format!(
            "jpeg2000: SIZ segment too short ({} bytes, need >= 36)",
            seg.len()
        )));
    }
    let rsiz = u16::from_be_bytes([seg[0], seg[1]]);
    let xsiz = u32::from_be_bytes([seg[2], seg[3], seg[4], seg[5]]);
    let ysiz = u32::from_be_bytes([seg[6], seg[7], seg[8], seg[9]]);
    let xosiz = u32::from_be_bytes([seg[10], seg[11], seg[12], seg[13]]);
    let yosiz = u32::from_be_bytes([seg[14], seg[15], seg[16], seg[17]]);
    let xtsiz = u32::from_be_bytes([seg[18], seg[19], seg[20], seg[21]]);
    let ytsiz = u32::from_be_bytes([seg[22], seg[23], seg[24], seg[25]]);
    let xtosiz = u32::from_be_bytes([seg[26], seg[27], seg[28], seg[29]]);
    let ytosiz = u32::from_be_bytes([seg[30], seg[31], seg[32], seg[33]]);
    let csiz = u16::from_be_bytes([seg[34], seg[35]]) as usize;

    let comp_bytes = csiz
        .checked_mul(3)
        .ok_or_else(|| Error::invalid("jpeg2000: SIZ component count overflow"))?;
    if seg.len() < 36 + comp_bytes {
        return Err(Error::invalid(format!(
            "jpeg2000: SIZ segment truncated: have {} bytes, need {} for {} components",
            seg.len(),
            36 + comp_bytes,
            csiz
        )));
    }
    let mut components = Vec::with_capacity(csiz);
    for i in 0..csiz {
        let off = 36 + i * 3;
        components.push(ComponentInfo {
            ssiz: seg[off],
            xrsiz: seg[off + 1],
            yrsiz: seg[off + 2],
        });
    }

    Ok(Siz {
        rsiz,
        xsiz,
        ysiz,
        xosiz,
        yosiz,
        xtsiz,
        ytsiz,
        xtosiz,
        ytosiz,
        components,
    })
}

/// Walk forward from `start` until we find either another SOT (FF 90) or
/// the EOC (FF D9). Used when a tile-part's Psot is 0 ("length unknown,
/// runs to the next tile-part or end"). The scan skips every 0xFF that
/// is part of a two-byte marker.
fn scan_to_next_boundary(buf: &[u8], start: usize) -> Result<usize> {
    let mut i = start;
    while i + 1 < buf.len() {
        if buf[i] == 0xFF {
            let m = u16::from_be_bytes([buf[i], buf[i + 1]]);
            if m == Marker::SOT.0 || m == Marker::EOC.0 {
                return Ok(i - start);
            }
        }
        i += 1;
    }
    Err(Error::invalid(
        "jpeg2000: tile-part with Psot=0 never reaches SOT or EOC",
    ))
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn pos(&self) -> usize {
        self.pos
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn read_marker(&mut self) -> Result<Marker> {
        if self.remaining() < 2 {
            return Err(Error::invalid(
                "jpeg2000: truncated codestream while reading marker",
            ));
        }
        let m = u16::from_be_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
        self.pos += 2;
        Ok(Marker(m))
    }

    fn read_u16(&mut self) -> Result<u16> {
        if self.remaining() < 2 {
            return Err(Error::invalid(
                "jpeg2000: truncated codestream while reading u16",
            ));
        }
        let v = u16::from_be_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    /// Read a length-prefixed marker segment payload. The returned slice
    /// is the `Lseg - 2` bytes that follow the length field.
    fn read_len_segment(&mut self) -> Result<&'a [u8]> {
        let lseg = self.read_u16()? as usize;
        if lseg < 2 {
            return Err(Error::invalid(format!(
                "jpeg2000: marker segment length must be >= 2, got {lseg}"
            )));
        }
        let body_len = lseg - 2;
        if self.remaining() < body_len {
            return Err(Error::invalid(format!(
                "jpeg2000: marker segment body {body_len} > remaining {}",
                self.remaining()
            )));
        }
        let slice = &self.buf[self.pos..self.pos + body_len];
        self.pos += body_len;
        Ok(slice)
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        if self.remaining() < n {
            return Err(Error::invalid(format!(
                "jpeg2000: skip {n} bytes but only {} remain",
                self.remaining()
            )));
        }
        self.pos += n;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal hand-crafted J2K codestream: one 4x3 grayscale
    /// tile with zero compressed data. Valid enough for the marker
    /// parser; decode would fail because the tier-1 payload is empty.
    fn build_tiny_j2k() -> Vec<u8> {
        let mut v = Vec::new();
        // SOC
        v.extend_from_slice(&[0xFF, 0x4F]);
        // SIZ — Lsiz = 38 + 3 * Csiz; Csiz = 1 → Lsiz = 41
        v.extend_from_slice(&[0xFF, 0x51]);
        v.extend_from_slice(&41u16.to_be_bytes()); // Lsiz
        v.extend_from_slice(&0u16.to_be_bytes()); // Rsiz = unrestricted
        v.extend_from_slice(&4u32.to_be_bytes()); // Xsiz
        v.extend_from_slice(&3u32.to_be_bytes()); // Ysiz
        v.extend_from_slice(&0u32.to_be_bytes()); // XOsiz
        v.extend_from_slice(&0u32.to_be_bytes()); // YOsiz
        v.extend_from_slice(&4u32.to_be_bytes()); // XTsiz
        v.extend_from_slice(&3u32.to_be_bytes()); // YTsiz
        v.extend_from_slice(&0u32.to_be_bytes()); // XTOsiz
        v.extend_from_slice(&0u32.to_be_bytes()); // YTOsiz
        v.extend_from_slice(&1u16.to_be_bytes()); // Csiz
        v.extend_from_slice(&[7, 1, 1]); // Ssiz = 8-bit unsigned, no subsampling
                                         // COD (Lcod = 12, 10 bytes payload)
        v.extend_from_slice(&[0xFF, 0x52]);
        v.extend_from_slice(&12u16.to_be_bytes());
        v.extend_from_slice(&[
            0, // Scod
            0, 0, 0, 0, // SGcod
            5, // num decomposition levels
            4, 4, 0, 0, // SPcod remainder
        ]);
        // QCD (Lqcd = 5, 3 bytes payload)
        v.extend_from_slice(&[0xFF, 0x5C]);
        v.extend_from_slice(&5u16.to_be_bytes());
        v.extend_from_slice(&[0x00, 0x00, 0x00]);
        // SOT (Lsot = 10, 8 bytes payload). Psot we fill in below.
        let sot_marker_off = v.len();
        v.extend_from_slice(&[0xFF, 0x90]);
        v.extend_from_slice(&10u16.to_be_bytes());
        v.extend_from_slice(&0u16.to_be_bytes()); // Isot = tile 0
        let psot_pos = v.len();
        v.extend_from_slice(&0u32.to_be_bytes()); // Psot — patched later
        v.extend_from_slice(&[0, 1]); // TPsot = 0, TNsot = 1
                                      // SOD (no length) + 2 body bytes (pretend compressed data).
        v.extend_from_slice(&[0xFF, 0x93]);
        let body: [u8; 2] = [0x00, 0x00];
        v.extend_from_slice(&body);
        // Patch Psot = total bytes from SOT marker through end of body.
        let tile_part_end = v.len();
        let psot = (tile_part_end - sot_marker_off) as u32;
        v[psot_pos..psot_pos + 4].copy_from_slice(&psot.to_be_bytes());
        // EOC
        v.extend_from_slice(&[0xFF, 0xD9]);
        v
    }

    #[test]
    fn parses_minimal_codestream() {
        let buf = build_tiny_j2k();
        let cs = parse(&buf).expect("parse");
        assert_eq!(cs.siz.image_width(), 4);
        assert_eq!(cs.siz.image_height(), 3);
        assert_eq!(cs.siz.num_components(), 1);
        assert_eq!(cs.siz.components[0].bit_depth(), 8);
        assert!(!cs.siz.components[0].is_signed());
        assert!(cs.cod.is_some());
        assert!(cs.qcd.is_some());
        assert_eq!(cs.tile_parts.len(), 1);
        let tp = cs.tile_parts[0];
        assert_eq!(tp.tile_index, 0);
        assert_eq!(tp.tile_part_index, 0);
        assert_eq!(tp.tile_part_count, 1);
        assert_eq!(tp.sod_length, 2);
        assert!(cs.eoc_offset.is_some());
    }

    #[test]
    fn rejects_missing_soc() {
        let buf = [0xFF, 0x51, 0x00, 0x00];
        assert!(parse(&buf).is_err());
    }

    #[test]
    fn rejects_truncated_siz() {
        let buf = [0xFF, 0x4F, 0xFF, 0x51, 0x00, 0x10];
        assert!(parse(&buf).is_err());
    }

    #[test]
    fn handles_psot_zero() {
        let mut buf = build_tiny_j2k();
        // Find SOT, zero its Psot so the parser has to scan to EOC.
        let sot_pos = buf.windows(2).position(|w| w == [0xFF, 0x90]).unwrap();
        let psot_pos = sot_pos + 4;
        buf[psot_pos..psot_pos + 4].copy_from_slice(&0u32.to_be_bytes());
        let cs = parse(&buf).expect("parse with Psot=0");
        assert_eq!(cs.tile_parts.len(), 1);
        assert_eq!(cs.tile_parts[0].sod_length, 2);
    }

    #[test]
    fn component_bit_depth_and_sign() {
        // ssiz = 0x8F → signed, bit depth 16
        let c = ComponentInfo {
            ssiz: 0x8F,
            xrsiz: 1,
            yrsiz: 1,
        };
        assert!(c.is_signed());
        assert_eq!(c.bit_depth(), 16);
    }
}
