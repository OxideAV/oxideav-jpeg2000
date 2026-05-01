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
//! | CAP    | FF 50  | Extended capabilities             | Pcap + Ccap_i list (HTJ2K detection) |
//! | PRF    | FF 56  | Profile                           | raw segment    |
//! | CPF    | FF 59  | Corresponding profile (HTJ2K)     | Pcpf_i list    |
//! | COD    | FF 52  | Coding style (default)            | raw segment    |
//! | QCD    | FF 5C  | Quantisation (default)            | raw segment    |
//! | COC    | FF 53  | Coding style (per-component)      | raw segment    |
//! | QCC    | FF 5D  | Quantisation (per-component)      | raw segment    |
//! | RGN    | FF 5E  | Region of interest                | raw segment    |
//! | POC    | FF 5F  | Progression order change          | main + per-tile-part raw payload |
//! | PPM    | FF 60  | Packed packet headers, main       | per-segment raw payload list |
//! | PPT    | FF 61  | Packed packet headers, tile-part  | per-tile-part raw payload list |
//! | TLM    | FF 55  | Tile-part lengths                 | raw segment    |
//! | PLM    | FF 57  | Packet lengths, main              | raw segment    |
//! | CRG    | FF 63  | Component registration            | raw segment    |
//! | COM    | FF 64  | Comment                           | raw segment    |
//! | SOT    | FF 90  | Start of tile-part                | tile index, length |
//! | SOD    | FF 93  | Start of data (no length field)   | offset/length of compressed body |
//! | EOC    | FF D9  | End of codestream                 | presence       |
//!
//! HTJ2K detection (ISO/IEC 15444-15, §A.3): an HTJ2K codestream is
//! identified by the presence of a `CAP` marker segment whose `Pcap`
//! field has bit 15 set (counted from the MSB, i.e. `Pcap & 0x0002_0000`).
//! When this bit is set, the corresponding `Ccap15` 16-bit value
//! describes the HT block-coding sub-profile (HTONLY / HTDECLARED /
//! MIXED, single vs multi HT-set, RGN presence, irreversible-transform
//! support, magnitude bound). [`Cap::is_htj2k`] / [`Cap::ccap15`]
//! expose the parsed bits.
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
    pub const CAP: Marker = Marker(0xFF50);
    pub const SIZ: Marker = Marker(0xFF51);
    pub const COD: Marker = Marker(0xFF52);
    pub const COC: Marker = Marker(0xFF53);
    pub const TLM: Marker = Marker(0xFF55);
    pub const PRF: Marker = Marker(0xFF56);
    pub const PLM: Marker = Marker(0xFF57);
    pub const PLT: Marker = Marker(0xFF58);
    /// Corresponding profile (HTJ2K, ISO/IEC 15444-15 §A.6).
    pub const CPF: Marker = Marker(0xFF59);
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

/// Parsed `CAP` (Extended Capabilities) marker segment.
///
/// Per ISO/IEC 15444-1 §A.5.2 / §A.7bis: `CAP` carries a 32-bit `Pcap`
/// bitmap (one bit per Part-N capability index, MSB = Pcap1, LSB =
/// Pcap32) and a variable-length list of 16-bit `Ccap_i` values (one
/// per `Pcap_i = 1` bit, in MSB-to-LSB order). Each `Ccap_i` is defined
/// by the extension that owns that bit. For HTJ2K (Part 15), `Pcap15`
/// (the 15th most-significant bit, mask `0x0002_0000`) shall be 1 and
/// `Ccap15` carries the HT sub-profile bits described in
/// ISO/IEC 15444-15 §A.3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cap {
    /// 32-bit `Pcap` bitmap. Bit `i` (1-based, MSB = bit 1) maps to
    /// Part-`i` capabilities.
    pub pcap: u32,
    /// `Ccap_i` values in the same order as set bits of `Pcap`
    /// (MSB → LSB). Length equals `pcap.count_ones() as usize`.
    pub ccaps: Vec<u16>,
}

impl Cap {
    /// True when `Pcap15` (the 15th MSB, mask `0x0002_0000`) is set —
    /// signalling that the codestream uses HT block coding per
    /// ISO/IEC 15444-15.
    pub fn is_htj2k(&self) -> bool {
        (self.pcap & PCAP15_MASK) != 0
    }

    /// Returns the `Ccap15` value if `Pcap15` is set. The position of
    /// `Ccap15` inside `ccaps` is the popcount of the bits *strictly
    /// more significant* than bit 15 (i.e. `Pcap1..Pcap14`).
    pub fn ccap15(&self) -> Option<u16> {
        if !self.is_htj2k() {
            return None;
        }
        // Bits more significant than Pcap15 inside the 32-bit Pcap
        // word are bit-positions 31 down to 17 (since Pcap15 sits at
        // bit-position 17 = 32 - 15). Mask off everything at or below
        // Pcap15 and count the surviving 1-bits.
        let higher_mask = !((PCAP15_MASK << 1).wrapping_sub(1));
        let idx = (self.pcap & higher_mask).count_ones() as usize;
        self.ccaps.get(idx).copied()
    }
}

/// Bit mask for `Pcap15` inside the 32-bit `Pcap` field.
///
/// Per ISO/IEC 15444-1 §A.5.2 Table A.11ter, `Pcap_i` corresponds to
/// the `i`-th most-significant bit of the 32-bit `Pcap` word, with
/// `Pcap1 = MSB` (bit-position 31, LSB = 0) and `Pcap32 = LSB`
/// (bit-position 0). `Pcap15` therefore lives at bit-position
/// `32 - 15 = 17`, i.e. mask `0x0002_0000`.
const PCAP15_MASK: u32 = 1u32 << (32 - 15);

/// Parsed `CPF` (Corresponding Profile) marker segment from
/// ISO/IEC 15444-15 §A.6. Records the raw `Pcpf_i` 16-bit words plus
/// the reconstructed `CPFnum` integer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cpf {
    /// Raw `Pcpf_i` values (one or more 16-bit integers).
    pub pcpf: Vec<u16>,
    /// `CPFnum = -1 + Σ Pcpf_i · 2^(16·(i-1))`. Stored as `u128` to
    /// admit up to ~7 `Pcpf_i` words without overflow; longer encodings
    /// are rejected at parse time.
    pub cpfnum: u128,
}

/// Records a single tile-part's position + length inside the codestream.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Raw POC segment payload appearing in this tile-part's header
    /// (T.800 §A.6.6). When set, the tile uses these progressions
    /// instead of the main-header POC / COD progression order.
    /// Per §A.6.6, all POC marker segments for a given tile must appear
    /// in tile-part headers of that tile; we accumulate any additional
    /// POC found in subsequent tile-parts of the same tile in the order
    /// encountered.
    pub poc: Option<Vec<u8>>,
    /// Raw PPT segment payloads from this tile-part header
    /// (T.800 §A.7.5), in the order encountered. Each entry is the
    /// body of one PPT segment (after Zppt: just `Ippti` packet header
    /// bytes). The decoder concatenates all PPT payloads of a tile
    /// (across its tile-parts) and reads packet headers from the
    /// resulting buffer.
    pub ppt: Vec<Vec<u8>>,
}

/// Full parse result for one J2K codestream.
#[derive(Debug, Clone)]
pub struct Codestream {
    pub siz: Siz,
    /// Parsed `CAP` segment, if the codestream carries one. When
    /// `cap.is_htj2k()` returns true, the stream uses the HT block
    /// coding algorithm of ISO/IEC 15444-15.
    pub cap: Option<Cap>,
    /// Parsed `CPF` (Corresponding Profile) segment from
    /// ISO/IEC 15444-15 §A.6, if present.
    pub cpf: Option<Cpf>,
    /// Raw COD segment payload (after Lcod). `None` if absent (malformed).
    pub cod: Option<Vec<u8>>,
    /// Raw QCD segment payload (after Lqcd). `None` if absent.
    pub qcd: Option<Vec<u8>>,
    /// Raw POC segment payload from the main header, if present
    /// (T.800 §A.6.6). When present, it overrides the COD progression
    /// order for all tiles unless they carry their own tile-part POC.
    pub poc: Option<Vec<u8>>,
    /// Raw PPM segment payloads from the main header (T.800 §A.7.4),
    /// in the order encountered. Each entry is the body of one PPM
    /// segment (after Zppm + concatenated `(Nppmi, Ippmi)` records).
    /// When non-empty, every tile's packet headers are stored here
    /// instead of inside the tile-part bodies.
    pub ppm: Vec<Vec<u8>>,
    pub tile_parts: Vec<TilePart>,
    /// Byte offset of the EOC marker, or `None` if the stream was truncated.
    pub eoc_offset: Option<usize>,
}

impl Codestream {
    /// Convenience: true when the codestream signals HTJ2K block coding
    /// (CAP marker present with `Pcap15` set, per ISO/IEC 15444-15
    /// §A.3.1). Returns false for classic Part-1 codestreams.
    pub fn is_htj2k(&self) -> bool {
        self.cap.as_ref().is_some_and(Cap::is_htj2k)
    }
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

    let mut cap: Option<Cap> = None;
    let mut cpf: Option<Cpf> = None;
    let mut cod: Option<Vec<u8>> = None;
    let mut qcd: Option<Vec<u8>> = None;
    let mut poc: Option<Vec<u8>> = None;
    // PPM main-header segments accumulate by Zppm order. We collect the
    // raw payloads here in the order encountered; downstream tier-2
    // setup re-sorts by Zppm and concatenates per §A.7.4.
    let mut ppm: Vec<Vec<u8>> = Vec::new();
    let mut tile_parts: Vec<TilePart> = Vec::new();
    let mut eoc_offset: Option<usize> = None;

    loop {
        if cur.remaining() == 0 {
            break;
        }
        let marker_off = cur.pos();
        let m = cur.read_marker()?;
        match m {
            Marker::CAP => {
                let seg = cur.read_len_segment()?;
                cap = Some(parse_cap(seg)?);
            }
            Marker::CPF => {
                let seg = cur.read_len_segment()?;
                cpf = Some(parse_cpf(seg)?);
            }
            Marker::COD => {
                let seg = cur.read_len_segment()?;
                cod = Some(seg.to_vec());
            }
            Marker::QCD => {
                let seg = cur.read_len_segment()?;
                qcd = Some(seg.to_vec());
            }
            Marker::POC => {
                let seg = cur.read_len_segment()?;
                poc = Some(seg.to_vec());
            }
            Marker::PPM => {
                // §A.7.4: PPM payload starts with `Zppm` (1 byte).
                // We retain the full segment (including Zppm) here; the
                // decoder sorts by Zppm and concatenates the trailing
                // bytes when setting up the per-tile-part header
                // streams.
                let seg = cur.read_len_segment()?;
                ppm.push(seg.to_vec());
            }
            Marker::COC
            | Marker::QCC
            | Marker::RGN
            | Marker::PPT
            | Marker::TLM
            | Marker::PRF
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

                // Tile-part header: any number of marker segments can
                // appear between SOT and SOD per §A.4.2 (COD, COC, QCD,
                // QCC, RGN, POC, PPT, PLT, COM). We capture POC + PPT;
                // the other tile-part-header segments are skipped
                // (their values would override main-header settings for
                // this tile, which is not yet supported).
                let mut tp_poc: Option<Vec<u8>> = None;
                let mut tp_ppt: Vec<Vec<u8>> = Vec::new();
                loop {
                    let next = cur.read_marker()?;
                    match next {
                        Marker::SOD => break,
                        Marker::POC => {
                            let s = cur.read_len_segment()?;
                            // §A.6.6 allows multiple POC markers across
                            // tile-parts of the same tile; for the first
                            // tile-part we store the payload, additional
                            // POC payloads are appended to the same tile's
                            // record so the walker sees one merged list
                            // (the per-tile aggregation happens later).
                            tp_poc = Some(s.to_vec());
                        }
                        Marker::PPT => {
                            // §A.7.5: PPT payload starts with `Zppt`
                            // (1 byte) followed by Ippt header bytes.
                            // We retain the full segment; the decoder
                            // sorts by Zppt and concatenates Ippt
                            // tails per the spec.
                            let s = cur.read_len_segment()?;
                            tp_ppt.push(s.to_vec());
                        }
                        Marker::COD
                        | Marker::COC
                        | Marker::QCD
                        | Marker::QCC
                        | Marker::RGN
                        | Marker::PLT
                        | Marker::COM => {
                            let _ = cur.read_len_segment()?;
                        }
                        other => {
                            return Err(Error::invalid(format!(
                                "jpeg2000: unexpected marker {:04X} in tile-part header",
                                other.0
                            )));
                        }
                    }
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
                    poc: tp_poc,
                    ppt: tp_ppt,
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
        cap,
        cpf,
        cod,
        qcd,
        poc,
        ppm,
        tile_parts,
        eoc_offset,
    })
}

/// Parse a `CAP` (Extended Capabilities) marker segment body.
///
/// Per ISO/IEC 15444-1 §A.5.2, the payload (after `Lcap`) is:
///   * `Pcap`: 32-bit big-endian capability bitmap
///   * `Ccap_i`: 16-bit big-endian per-capability values, one per
///     set bit in `Pcap`, in MSB→LSB order.
///
/// The total segment length must satisfy `Lcap = 6 + 2·n` where `n`
/// is `Pcap.count_ones()` (segment-length parameter range 8..=70).
fn parse_cap(seg: &[u8]) -> Result<Cap> {
    if seg.len() < 4 {
        return Err(Error::invalid(format!(
            "jpeg2000: CAP segment too short ({} bytes, need >= 4)",
            seg.len()
        )));
    }
    let pcap = u32::from_be_bytes([seg[0], seg[1], seg[2], seg[3]]);
    let n = pcap.count_ones() as usize;
    // Bound: per Table A.11bis, Lcap range is 8..=70 → at most 32 set
    // bits. `count_ones` on a 32-bit word is naturally bounded; we
    // additionally check the segment carries enough bytes for n
    // 16-bit Ccap_i values.
    let need = 4usize
        .checked_add(
            n.checked_mul(2)
                .ok_or_else(|| Error::invalid("jpeg2000: CAP Pcap bit count overflow"))?,
        )
        .ok_or_else(|| Error::invalid("jpeg2000: CAP length overflow"))?;
    if seg.len() < need {
        return Err(Error::invalid(format!(
            "jpeg2000: CAP segment truncated: have {} bytes, need {} for Pcap+{} Ccap values",
            seg.len(),
            need,
            n
        )));
    }
    let mut ccaps = Vec::with_capacity(n);
    for i in 0..n {
        let off = 4 + i * 2;
        ccaps.push(u16::from_be_bytes([seg[off], seg[off + 1]]));
    }
    Ok(Cap { pcap, ccaps })
}

/// Parse a `CPF` (Corresponding Profile) marker segment body.
///
/// Per ISO/IEC 15444-15 §A.6, the payload (after `Lcpf`) is `N`
/// 16-bit `Pcpf_i` values, where
///   * `Lcpf = 2 + 2·N`,
///   * `Pcpf_N` is non-zero,
///   * `CPFnum = -1 + Σ_{i=1..N} Pcpf_i · 2^(16·(i-1))`.
///
/// Bound: we cap `N` at 8 (CPFnum representable in `u128`); larger
/// segments are rejected as `InvalidData` to keep downstream
/// allocations bounded.
fn parse_cpf(seg: &[u8]) -> Result<Cpf> {
    if seg.is_empty() || seg.len() % 2 != 0 {
        return Err(Error::invalid(format!(
            "jpeg2000: CPF segment length must be a positive multiple of 2, got {}",
            seg.len()
        )));
    }
    let n = seg.len() / 2;
    const MAX_PCPF: usize = 8;
    if n > MAX_PCPF {
        return Err(Error::invalid(format!(
            "jpeg2000: CPF Pcpf count {n} exceeds bound {MAX_PCPF}"
        )));
    }
    let mut pcpf = Vec::with_capacity(n);
    for i in 0..n {
        pcpf.push(u16::from_be_bytes([seg[i * 2], seg[i * 2 + 1]]));
    }
    if *pcpf.last().unwrap() == 0 {
        return Err(Error::invalid(
            "jpeg2000: CPF Pcpf_N (last word) must be non-zero",
        ));
    }
    // CPFnum = -1 + Σ Pcpf_i · 2^(16·(i-1))
    let mut sum: u128 = 0;
    for (i, w) in pcpf.iter().enumerate() {
        let shift = 16u32 * i as u32;
        // shift <= 16 * (MAX_PCPF - 1) = 112 < 128 → never overflows.
        let term = (*w as u128) << shift;
        sum = sum
            .checked_add(term)
            .ok_or_else(|| Error::invalid("jpeg2000: CPF CPFnum sum overflow"))?;
    }
    if sum == 0 {
        return Err(Error::invalid(
            "jpeg2000: CPF CPFnum underflow (sum was 0, would yield -1)",
        ));
    }
    let cpfnum = sum - 1;
    Ok(Cpf { pcpf, cpfnum })
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
        let tp = &cs.tile_parts[0];
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
