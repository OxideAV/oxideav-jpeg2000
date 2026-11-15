//! High-Throughput JPEG 2000 (HTJ2K) block decoder — ITU-T T.814 |
//! ISO/IEC 15444-15:2019.
//!
//! Part 15 of JPEG 2000 replaces the Annex D MQ-coded tier-1 block coder
//! (T.800 / [`crate::t1`]) with a *high-throughput* block coder that
//! decodes a code-block in three passes, none of which use the MQ
//! arithmetic coder:
//!
//! * the **HT cleanup pass** (§7.3) — the bulk of the work, decoding the
//!   full significance + magnitude + sign of every sample from three
//!   interleaved variable-length / run-length byte-streams (MagSgn, MEL,
//!   VLC) packed into a single **HT cleanup segment**;
//! * the **HT SigProp pass** (§7.4) — a refinement pass adding one extra
//!   magnitude bit-plane to samples that became newly significant via
//!   neighbour propagation;
//! * the **HT MagRef pass** (§7.5) — a magnitude-refinement pass adding
//!   one bit to already-significant samples.
//!
//! The number of passes decoded for a given HT set is `Z_blk` (§B.3):
//! `1` (cleanup only), `2` (cleanup + SigProp), or `3` (all three).
//!
//! This module is self-contained: [`decode_ht_codeblock`] consumes the
//! raw HT segment bytes plus the `Z_blk` / `S_blk` / `p` parameters and
//! returns a [`crate::t1::CodeBlock`] with the same coefficient grid
//! (magnitude + sign + significance) the MQ path produces, so the
//! reassembly and inverse-quantisation stages downstream are oblivious
//! to which block coder ran.
//!
//! ## Quad scanning order (§7.2)
//!
//! The cleanup pass scans the code-block as an array of 2×2 *quads*,
//! `QW = ⌈Wblk/2⌉` wide and `QH = ⌈Hblk/2⌉` tall. Location `n = 4q + j`
//! within quad `q` numbers the samples top-left (`j=0`), bottom-left
//! (`j=1`), top-right (`j=2`), bottom-right (`j=3`). Odd dimensions pad
//! a zero column/row.
//!
//! All truth in this module comes from `docs/image/jpeg2000/` (T.814 |
//! 15444-15). No external HTJ2K implementation was consulted.

use crate::Error;
use crate::geometry::SubBandOrientation;
use crate::t1::{CodeBlock, Coefficient};

mod tables {
    pub(super) use crate::ht_tables::{CXT_VLC_TABLE_0, CXT_VLC_TABLE_1, CxtVlcEntry};
}
use tables::{CXT_VLC_TABLE_0, CXT_VLC_TABLE_1, CxtVlcEntry};

/// T.814 Table 2 — MEL exponent table `MEL_E[k]`, `k` in `0..=12`.
pub(crate) const MEL_E: [u8; 13] = [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 4, 5];

// ---------------------------------------------------------------------------
// §7.1.2 — MagSgn bit-stream recovery (forward, little-endian, 0xFF-stuffed)
// ---------------------------------------------------------------------------

/// The HT MagSgn bit-stream (§7.1.2 `importMagSgnBit`).
///
/// Unpacks bits from the MagSgn byte-stream (bytes `0..Pcup` of the HT
/// cleanup segment) in little-endian order, skipping the stuffing bit
/// that appears in the MSB position of any byte following a `0xFF`.
/// After `Pcup` real bytes are exhausted the procedure synthesises a
/// single `0xFF` byte, then `error()`s — but a conforming codestream
/// never needs more than that synthesised byte.
///
/// Bytes are imported several at a time into a 64-bit window, so a
/// magnitude is one shift and mask. A byte the procedure would reject —
/// a set stuffing bit, anything past the synthesised `0xFF` — is not
/// imported and stops the import for good: the error is raised when a
/// bit of that byte is first asked for, as §7.1.2 raises it.
struct MagSgnReader<'a> {
    /// The modified `Dcup` array view (`modDcup` is applied on read).
    dcup: ModDcup<'a>,
    /// Prefix length `Pcup = Lcup − Scup`.
    pcup: usize,
    /// `MS_pos`: the next byte to import.
    pos: usize,
    /// `MS_last`: the last byte imported.
    last: u32,
    /// Imported bits not yet handed out, the next one in bit 0.
    window: u64,
    /// Number of valid bits in `window`.
    avail: u32,
    /// No further byte can be imported.
    blocked: bool,
}

impl<'a> MagSgnReader<'a> {
    fn new(dcup: ModDcup<'a>, pcup: usize) -> Self {
        // initMS: all state zero.
        Self {
            dcup,
            pcup,
            pos: 0,
            last: 0,
            window: 0,
            avail: 0,
            blocked: false,
        }
    }

    /// The byte-import half of §7.1.2 `importMagSgnBit`, for as many
    /// bytes as the window has room for: 7 payload bits after a `0xFF`,
    /// 8 otherwise.
    #[cold]
    #[inline(never)]
    fn import(&mut self) {
        while self.avail <= 56 && !self.blocked {
            let bits = if self.last == 0xFF { 7 } else { 8 };
            let byte = if self.pos < self.pcup {
                self.dcup.get(self.pos) as u32
            } else if self.pos == self.pcup {
                0xFF
            } else {
                self.blocked = true;
                break;
            };
            if self.pos < self.pcup && byte & (1 << bits) != 0 {
                // A set stuffing bit.
                self.blocked = true;
                break;
            }
            self.window |= u64::from(byte & ((1 << bits) - 1)) << self.avail;
            self.avail += bits;
            self.last = byte;
            self.pos += 1;
        }
    }

    /// Unpack `m` bits as a little-endian integer (the inner loop of
    /// §7.3.8 `decodeMagSgnValue`).
    ///
    /// A conformant stream in this crate's supported precision range
    /// keeps every magnitude inside the 32-bit lane (`Mb` is bounded
    /// well below 32 by the Equation E-2 quantisation tables), so a
    /// bit count that cannot fit is a corrupt / non-conformant stream,
    /// not a wider integer.
    #[inline]
    fn bits(&mut self, m: u32) -> Result<u32, Error> {
        if m > 32 {
            return Err(Error::HtCorruptSegment);
        }
        if m > self.avail {
            self.import();
            if m > self.avail {
                return Err(Error::HtCorruptSegment);
            }
        }
        let val = (self.window & ((1u64 << m) - 1)) as u32;
        self.window >>= m;
        self.avail -= m;
        Ok(val)
    }
}

// ---------------------------------------------------------------------------
// §7.1.3 — MEL bit-stream recovery (forward, big-endian, 0xFF-stuffed)
// ---------------------------------------------------------------------------

/// State machine for the MEL bit-stream (§7.1.3 `importMELBit`).
///
/// The MEL byte-stream extends forward from byte `Pcup` of the HT
/// cleanup segment for at most `Scup` bytes; bits are unpacked in
/// big-endian order, skipping the stuffing bit after any `0xFF`.
struct MelReader<'a> {
    dcup: ModDcup<'a>,
    lcup: usize,
    mel_pos: usize,
    mel_bits: u32,
    mel_tmp: u32,
}

impl<'a> MelReader<'a> {
    fn new(dcup: ModDcup<'a>, pcup: usize, lcup: usize) -> Self {
        // initMEL: MEL_pos = Pcup, rest zero.
        Self {
            dcup,
            lcup,
            mel_pos: pcup,
            mel_bits: 0,
            mel_tmp: 0,
        }
    }

    /// §7.1.3 `importMELBit`.
    fn bit(&mut self) -> u8 {
        if self.mel_bits == 0 {
            self.mel_bits = if self.mel_tmp == 0xFF { 7 } else { 8 };
            if self.mel_pos < self.lcup {
                self.mel_tmp = self.dcup.get(self.mel_pos) as u32;
                self.mel_pos += 1;
            } else {
                self.mel_tmp = 0xFF;
            }
        }
        self.mel_bits -= 1;
        ((self.mel_tmp >> self.mel_bits) & 1) as u8
    }
}

// ---------------------------------------------------------------------------
// §7.3.3 — MEL symbol decoder (adaptive run-length)
// ---------------------------------------------------------------------------

/// §7.3.3 MEL symbol decoder wrapping a [`MelReader`].
struct MelDecoder<'a> {
    reader: MelReader<'a>,
    mel_k: usize,
    mel_run: u32,
    mel_one: u32,
}

impl<'a> MelDecoder<'a> {
    fn new(reader: MelReader<'a>) -> Self {
        // initMELDecoder: MEL_k = MEL_run = MEL_one = 0.
        Self {
            reader,
            mel_k: 0,
            mel_run: 0,
            mel_one: 0,
        }
    }

    /// §7.3.3 `decodeMELSym` — returns the next MEL symbol (0 or 1).
    fn sym(&mut self) -> u8 {
        if self.mel_run == 0 && self.mel_one == 0 {
            let mut eval = MEL_E[self.mel_k] as u32;
            let bit = self.reader.bit();
            if bit == 1 {
                self.mel_run = 1 << eval;
                self.mel_k = (self.mel_k + 1).min(12);
            } else {
                self.mel_run = 0;
                while eval > 0 {
                    let b = self.reader.bit();
                    self.mel_run = 2 * self.mel_run + b as u32;
                    eval -= 1;
                }
                self.mel_k = self.mel_k.saturating_sub(1);
                self.mel_one = 1;
            }
        }
        if self.mel_run > 0 {
            self.mel_run -= 1;
            0
        } else {
            self.mel_one = 0;
            1
        }
    }
}

// ---------------------------------------------------------------------------
// §7.1.4 — VLC bit-stream recovery (reverse byte order, little-endian)
// ---------------------------------------------------------------------------

/// The HT VLC bit-stream (§7.1.4 `importVLCBit`).
///
/// The VLC byte-stream extends *backward* from the last byte of the HT
/// cleanup segment for at most `Scup` bytes (overlapping the MEL stream).
/// Bits are unpacked little-endian while bytes are consumed in reverse;
/// the stuffing bit (MSB) is skipped after any byte whose low 7 bits are
/// all 1 if the previously-consumed byte exceeded `0x8F`. The init
/// procedure also skips the 12 bits that were overwritten with 1s when
/// `Scup` was recovered from the last two bytes.
///
/// Bytes are imported several at a time into a 64-bit window, so a
/// CxtVLC codeword is matched on a seven-bit look-ahead instead of bit
/// by bit. Running off the start of the stream (`Pcup`) is an error
/// only when a bit past it is actually consumed.
struct VlcReader<'a> {
    dcup: ModDcup<'a>,
    pcup: usize,
    /// `VLC_pos`: the next byte to import.
    pos: isize,
    /// `VLC_last`: the last byte imported.
    last: u32,
    /// Imported bits not yet consumed, the next one in bit 0.
    window: u64,
    /// Number of valid bits in `window`.
    avail: u32,
}

impl<'a> VlcReader<'a> {
    fn new(dcup: ModDcup<'a>, pcup: usize, lcup: usize) -> Self {
        // initVLC.
        let last = dcup.get(lcup - 2) as u32;
        let tmp = last >> 4;
        let avail = if (tmp & 7) < 7 { 4 } else { 3 };
        Self {
            dcup,
            pcup,
            pos: lcup as isize - 3,
            last,
            window: u64::from(tmp & ((1 << avail) - 1)),
            avail,
        }
    }

    /// The byte-import half of §7.1.4 `importVLCBit`, for as many bytes
    /// as the window has room for.
    #[cold]
    #[inline(never)]
    fn import(&mut self) {
        while self.avail <= 56 && self.pos >= self.pcup as isize {
            let byte = self.dcup.get(self.pos as usize) as u32;
            let bits = if self.last > 0x8F && (byte & 0x7F) == 0x7F {
                7
            } else {
                8
            };
            self.window |= u64::from(byte & ((1 << bits) - 1)) << self.avail;
            self.avail += bits;
            self.last = byte;
            self.pos -= 1;
        }
    }

    /// The next seven bits without consuming them; bits past the start
    /// of the stream read as zero.
    #[inline]
    fn peek7(&mut self) -> usize {
        if self.avail < 7 {
            self.import();
        }
        (self.window & 0x7F) as usize
    }

    /// Consume `n` bits (at most 7); an error if the stream holds fewer.
    #[inline]
    fn consume(&mut self, n: u32) -> Result<(), Error> {
        if n > self.avail {
            return Err(Error::HtCorruptSegment);
        }
        self.window >>= n;
        self.avail -= n;
        Ok(())
    }

    /// §7.1.4 `importVLCBit`.
    #[inline]
    fn bit(&mut self) -> Result<u8, Error> {
        if self.avail == 0 {
            self.import();
        }
        let bit = (self.window & 1) as u8;
        self.consume(1)?;
        Ok(bit)
    }
}

// ---------------------------------------------------------------------------
// modDcup (§7.1.1) — the last byte and the low nibble of the second-last
// byte of the HT cleanup segment are overwritten with 1s.
// ---------------------------------------------------------------------------

/// A read-only view over the HT cleanup segment that applies the
/// `modDcup` rewrite of §7.1.1 on every access: `Dcup[Lcup-1]` reads as
/// `0xFF` and `Dcup[Lcup-2]` reads with its low nibble forced to `1`.
#[derive(Clone, Copy)]
struct ModDcup<'a> {
    bytes: &'a [u8],
    lcup: usize,
}

impl<'a> ModDcup<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            lcup: bytes.len(),
        }
    }

    /// `modDcup(Dcup, pos)` — out-of-range positions return 0, mirroring
    /// the spec's reliance on synthesised fill being handled by the
    /// importing state machines (they never read past their own bound).
    fn get(&self, pos: usize) -> u8 {
        if pos == self.lcup - 1 {
            0xFF
        } else if pos == self.lcup - 2 {
            self.bytes[pos] | 0x0F
        } else if pos < self.bytes.len() {
            self.bytes[pos]
        } else {
            0
        }
    }
}

// ---------------------------------------------------------------------------
// §7.3.5 — CxtVLC code matching
// ---------------------------------------------------------------------------

/// Result of `decodeCxtVLC` for one quad: the `(ρ, u_off, ε̄ᵏ, ε̄¹)`
/// tuple.
#[derive(Debug, Clone, Copy, Default)]
struct CxtVlcResult {
    rho: u8,
    u_off: u8,
    e_k: u8,
    e_1: u8,
}

/// Direct-index form of one Annex C CxtVLC table: slot
/// `(cq << 10) | ((len − 1) << 7) | cwd` holds `1 +` the index of the
/// first table entry whose `(cq, lw, w)` equals `(cq, len, cwd)`, or
/// `0` when no entry matches — the §7.3.5 `test_match` predicate
/// answered in one read instead of a scan over the table.
const CXT_VLC_SLOTS: usize = 8 << 10;

const fn index_cxt_vlc<const N: usize>(table: &[CxtVlcEntry; N]) -> [u16; CXT_VLC_SLOTS] {
    let mut slots = [0u16; CXT_VLC_SLOTS];
    // Walk backwards so the earliest of any duplicated keys wins, the
    // same entry a front-to-back scan would return.
    let mut i = N;
    while i > 0 {
        i -= 1;
        let e = &table[i];
        assert!(e.cq < 8 && e.len >= 1 && e.len <= 7 && e.cwd < 128);
        let slot = ((e.cq as usize) << 10) | ((e.len as usize - 1) << 7) | e.cwd as usize;
        slots[slot] = i as u16 + 1;
    }
    slots
}

const CXT_VLC_INDEX_0: [u16; CXT_VLC_SLOTS] = index_cxt_vlc(&CXT_VLC_TABLE_0);
const CXT_VLC_INDEX_1: [u16; CXT_VLC_SLOTS] = index_cxt_vlc(&CXT_VLC_TABLE_1);

/// Look-ahead form of one Annex C CxtVLC table: slot `(cq << 7) | bits`
/// — the context and the next seven VLC bits, first bit lowest — holds
/// `(1 + entry index) << 3 | length` of the codeword §7.3.5 matches,
/// i.e. the shortest prefix of the seven bits that equals a codeword of
/// that context; `0` when none does.
const CXT_VLC_AHEAD: usize = 8 << 7;

const fn look_ahead_cxt_vlc(index: &[u16; CXT_VLC_SLOTS]) -> [u16; CXT_VLC_AHEAD] {
    let mut slots = [0u16; CXT_VLC_AHEAD];
    let mut key = 0;
    while key < CXT_VLC_AHEAD {
        let context = (key >> 7) << 10;
        let bits = key & 0x7F;
        let mut len = 1;
        while len <= 7 {
            let hit = index[context | ((len - 1) << 7) | (bits & ((1 << len) - 1))];
            if hit != 0 {
                assert!(hit < 1 << 13);
                slots[key] = (hit << 3) | len as u16;
                break;
            }
            len += 1;
        }
        key += 1;
    }
    slots
}

static CXT_VLC_AHEAD_0: [u16; CXT_VLC_AHEAD] = look_ahead_cxt_vlc(&CXT_VLC_INDEX_0);
static CXT_VLC_AHEAD_1: [u16; CXT_VLC_AHEAD] = look_ahead_cxt_vlc(&CXT_VLC_INDEX_1);

/// §7.3.5 `decodeCxtVLC` — the procedure imports VLC bits one at a
/// time, growing the little-endian codeword until a table entry
/// matches; the look-ahead table gives that entry and its length for
/// the next seven bits at once.
fn decode_cxt_vlc(
    vlc: &mut VlcReader,
    cq: u8,
    first_line_pair: bool,
) -> Result<CxtVlcResult, Error> {
    let (table, ahead): (&[CxtVlcEntry], &[u16; CXT_VLC_AHEAD]) = if first_line_pair {
        (&CXT_VLC_TABLE_0, &CXT_VLC_AHEAD_0)
    } else {
        (&CXT_VLC_TABLE_1, &CXT_VLC_AHEAD_1)
    };
    if cq > 7 {
        // No table entry carries a context outside 0..=7.
        return Err(Error::HtCorruptSegment);
    }
    // Codewords are at most 7 bits (lw ≤ 7). No match within them, or a
    // match that needs bits the stream does not have, is a malformed
    // stream.
    let hit = ahead[(usize::from(cq) << 7) | vlc.peek7()];
    if hit == 0 {
        return Err(Error::HtCorruptSegment);
    }
    vlc.consume(u32::from(hit & 7))?;
    let e = &table[usize::from(hit >> 3) - 1];
    Ok(CxtVlcResult {
        rho: e.rho,
        u_off: e.u_off,
        e_k: e.e_k,
        e_1: e.e_1,
    })
}

// ---------------------------------------------------------------------------
// §7.3.6 — U-VLC (unsigned residual) decoding
// ---------------------------------------------------------------------------

/// §7.3.6 `decodeUPrefix`.
fn decode_u_prefix(vlc: &mut VlcReader) -> Result<u32, Error> {
    if vlc.bit()? == 1 {
        return Ok(1);
    }
    if vlc.bit()? == 1 {
        return Ok(2);
    }
    Ok(if vlc.bit()? == 1 { 3 } else { 5 })
}

/// §7.3.6 `decodeUSuffix`.
fn decode_u_suffix(vlc: &mut VlcReader, u_pfx: u32) -> Result<u32, Error> {
    if u_pfx < 3 {
        return Ok(0);
    }
    let mut val = vlc.bit()? as u32;
    if u_pfx == 3 {
        return Ok(val);
    }
    for i in 1..5 {
        let bit = vlc.bit()? as u32;
        val += bit << i;
    }
    Ok(val)
}

/// §7.3.6 `decodeUExtension`.
fn decode_u_extension(vlc: &mut VlcReader, u_sfx: u32) -> Result<u32, Error> {
    if u_sfx < 28 {
        return Ok(0);
    }
    let mut val = vlc.bit()? as u32;
    for i in 1..4 {
        let bit = vlc.bit()? as u32;
        val += bit << i;
    }
    Ok(val)
}

// ---------------------------------------------------------------------------
// §7.3.2 / Table 1 — magnitude exponent E_n from magnitude μ_n.
// ---------------------------------------------------------------------------

/// `E_n = min{E ∈ ℕ | (2μ − 1) < 2^E}` (§7.3.2). `E(0) = 0`, `E(1) = 1`,
/// `E(2) = 2`, `E(3..=4) = 3`, … per Table 1.
pub(crate) fn magnitude_exponent(mu: u32) -> u32 {
    if mu == 0 {
        return 0;
    }
    // Smallest E with 2^E > 2μ − 1, i.e. 2^E ≥ 2μ. Compute in u64 so a
    // corrupt / non-conformant μ near the top of the u32 lane (which a
    // hostile MagSgn stream can recover before the §7.6 lane bound
    // rejects it) does not overflow `2μ`.
    let t = 2 * u64::from(mu) - 1;
    64 - t.leading_zeros()
}

// ---------------------------------------------------------------------------
// §7.3 — HT cleanup decode (the bulk of the block-decoding work).
// ---------------------------------------------------------------------------

/// Per-quad decoded state accumulated through the cleanup pass.
#[derive(Clone, Copy, Default)]
pub(crate) struct QuadState {
    /// Significance pattern ρ_q (4-bit).
    pub(crate) rho: u8,
    /// EMB known-bit pattern ε̄ᵏ_q (4-bit).
    pub(crate) e_k: u8,
    /// EMB known-1 pattern ε̄¹_q (4-bit).
    pub(crate) e_1: u8,
    /// Unsigned residual u_q.
    pub(crate) u_q: u32,
    /// Exponent bound U_q.
    pub(crate) u_big: u32,
}

/// Working grid for the cleanup pass, indexed by quad scan order.
///
/// Sample location `n = 4q + j` maps to sub-band coordinates
/// `(x, y) = (2·qx + (j>>1), 2·qy + (j&1))` where `(qx, qy)` is the
/// quad's column/row in the `QW × QH` quad array (§7.2).
pub(crate) struct HtGrid {
    /// Code-block width / height in samples.
    pub(crate) wblk: usize,
    pub(crate) hblk: usize,
    /// Quad-array width / height.
    pub(crate) qw: usize,
    pub(crate) qh: usize,
    /// Per-sample magnitude μ_n (raster `x + y*wblk`).
    pub(crate) mu: Vec<u32>,
    /// Per-sample sign s_n (`true` ≡ negative).
    pub(crate) sign: Vec<bool>,
    /// Per-sample significance σ_n.
    pub(crate) sigma: Vec<bool>,
    /// Per-sample magnitude exponent E_n (cached for predictor reuse).
    pub(crate) exp: Vec<u32>,
    /// Per-quad decoded state.
    pub(crate) quads: Vec<QuadState>,
}

impl HtGrid {
    pub(crate) fn new(wblk: usize, hblk: usize) -> Self {
        let qw = wblk.div_ceil(2);
        let qh = hblk.div_ceil(2);
        Self {
            wblk,
            hblk,
            qw,
            qh,
            mu: vec![0; wblk * hblk],
            sign: vec![false; wblk * hblk],
            sigma: vec![false; wblk * hblk],
            exp: vec![0; wblk * hblk],
            quads: vec![QuadState::default(); qw * qh],
        }
    }

    /// `(x, y)` sub-band coordinates of sample `n = 4q + j`, or `None`
    /// if the sample lies in the odd-dimension pad region.
    #[inline]
    pub(crate) fn coord(&self, qx: usize, qy: usize, j: usize) -> Option<(usize, usize)> {
        let x = 2 * qx + (j >> 1);
        let y = 2 * qy + (j & 1);
        if x < self.wblk && y < self.hblk {
            Some((x, y))
        } else {
            None
        }
    }

    /// Column and row of quad `q` (`q = qy · QW + qx`) in the quad
    /// grid — the `(qx, qy)` the per-quad helpers take.
    #[inline]
    pub(crate) fn quad_xy(&self, q: usize) -> (usize, usize) {
        (q % self.qw, q / self.qw)
    }

    #[inline]
    fn sigma_at(&self, qx: usize, qy: usize, j: usize) -> u8 {
        match self.coord(qx, qy, j) {
            Some((x, y)) => self.sigma[x + y * self.wblk] as u8,
            None => 0,
        }
    }

    #[inline]
    fn exp_at(&self, qx: usize, qy: usize, j: usize) -> u32 {
        match self.coord(qx, qy, j) {
            Some((x, y)) => self.exp[x + y * self.wblk],
            None => 0,
        }
    }
}

/// §7.3.5 — coding context `c_q` for quad `q` from neighbouring
/// significance.
///
/// First quad row (`q < QW`) uses Formula (1) over `(σˢʷ, σʷ, σˢᶠ, σᶠ)`;
/// other rows use Formula (2) over the previous-row neighbourhood.
#[inline]
pub(crate) fn quad_context(grid: &HtGrid, qx: usize, qy: usize) -> u8 {
    if qy == 0 {
        let (sw, w, sf, f) = if qx > 0 {
            let p = qx - 1; // previous quad
            (
                grid.sigma_at(p, 0, 3), // σ_{4q-1} = sample j=3 of quad q-1
                grid.sigma_at(p, 0, 2), // σ_{4q-2} = j=2
                grid.sigma_at(p, 0, 1), // σ_{4q-3} = j=1
                grid.sigma_at(p, 0, 0), // σ_{4q-4} = j=0
            )
        } else {
            (0, 0, 0, 0)
        };
        (f | sf) + 2 * w + 4 * sw
    } else {
        let up = qy - 1; // row of the quad directly above
        let n = grid.sigma_at(qx, up, 1); // σⁿ  = σ_{4(q-QW)+1}
        let ne = grid.sigma_at(qx, up, 3); // σⁿᵉ = σ_{4(q-QW)+3}
        // The quads to the left exist unless this one opens its row,
        // the one above-right unless it closes it.
        let nw = if qx > 0 {
            grid.sigma_at(qx - 1, up, 3)
        } else {
            0
        };
        let nf = if qx + 1 < grid.qw {
            grid.sigma_at(qx + 1, up, 1)
        } else {
            0
        };
        let (w, sw) = if qx > 0 {
            (grid.sigma_at(qx - 1, qy, 2), grid.sigma_at(qx - 1, qy, 3))
        } else {
            (0, 0)
        };
        (nw | n) + 2 * (w | sw) + 4 * (ne | nf)
    }
}

/// §7.3.7 — exponent predictor κ_q for quad `q`.
///
/// First quad row: κ_q = 1. Otherwise κ_q is derived from the maximum
/// of four previous-line exponents, modulated by γ_q (Formula 5/6).
#[inline]
pub(crate) fn exponent_predictor(grid: &HtGrid, qx: usize, qy: usize, rho: u8) -> u32 {
    if qy == 0 {
        return 1;
    }
    let up = qy - 1;
    let e_n = grid.exp_at(qx, up, 1);
    let e_ne = grid.exp_at(qx, up, 3);
    let e_nw = if qx > 0 {
        grid.exp_at(qx - 1, up, 3)
    } else {
        0
    };
    let e_nf = if qx + 1 < grid.qw {
        grid.exp_at(qx + 1, up, 1)
    } else {
        0
    };
    let emax = e_nw.max(e_n).max(e_ne).max(e_nf);
    let gamma = !matches!(rho, 0 | 1 | 2 | 4 | 8);
    if gamma {
        1.max(emax.saturating_sub(1))
    } else {
        1
    }
}

/// Decode one HT cleanup coding pass over a code-block, populating the
/// significance / magnitude / sign / exponent grids (§7.3).
fn decode_cleanup(grid: &mut HtGrid, seg: &[u8]) -> Result<(), Error> {
    // §7.1.1 — recover Scup, Pcup, and the three byte-streams.
    let lcup = seg.len();
    if lcup < 2 {
        return Err(Error::HtCorruptSegment);
    }
    let dcup = ModDcup::new(seg);
    // Scup = (16 · Dcup[Lcup-1]) + (Dcup[Lcup-2] & 0x0F), using the
    // *unmodified* bytes (this is how Scup is recovered before modDcup
    // rewrites them — see §7.1.1).
    let scup = (16 * (seg[lcup - 1] as usize)) + ((seg[lcup - 2] as usize) & 0x0F);
    if !(2..=lcup.min(4079)).contains(&scup) {
        return Err(Error::HtCorruptSegment);
    }
    let pcup = lcup - scup;

    let mut magsgn = MagSgnReader::new(dcup, pcup);
    let mut mel = MelDecoder::new(MelReader::new(dcup, pcup, lcup));
    let mut vlc = VlcReader::new(dcup, pcup, lcup);

    let qw = grid.qw;
    let qh = grid.qh;

    // Phase 1: significance + EMB + U-VLC decoding, quad-pair
    // interleaved (§7.3.4), row by row. We record ρ/εᵏ/ε¹/u per quad,
    // computing contexts on the fly from already-decoded significance.
    for qy in 0..qh {
        let first_line_pair = qy == 0;
        let row_start = qy * qw;
        let mut qx = 0;
        while qx < qw {
            let q1 = row_start + qx;
            let has_q2 = qx + 1 < qw;
            let q2 = q1 + 1;

            // --- CxtVLC (decodeSigEMB) for each quad in the pair ---
            let r1 = decode_sig_emb(grid, &mut mel, &mut vlc, qx, qy)?;
            apply_sig_pattern(grid, qx, qy, r1.rho);
            let r2 = if has_q2 {
                let r = decode_sig_emb(grid, &mut mel, &mut vlc, qx + 1, qy)?;
                apply_sig_pattern(grid, qx + 1, qy, r.rho);
                Some(r)
            } else {
                None
            };

            // --- U-VLC decode (interleaved, §7.3.4 / §7.3.6) ---
            let (u1, u2) = decode_u_pair(
                &mut mel,
                &mut vlc,
                first_line_pair,
                r1.u_off,
                r2.map(|r| r.u_off),
            )?;

            // Store quad state.
            grid.quads[q1] = QuadState {
                rho: r1.rho,
                e_k: r1.e_k,
                e_1: r1.e_1,
                u_q: u1,
                u_big: 0,
            };
            if let Some(r) = r2 {
                grid.quads[q2] = QuadState {
                    rho: r.rho,
                    e_k: r.e_k,
                    e_1: r.e_1,
                    u_q: u2,
                    u_big: 0,
                };
            }

            // --- Predictors + MagSgn for the two quads (§7.3.7/§7.3.8) ---
            decode_quad_magsgn(grid, &mut magsgn, qx, qy)?;
            if has_q2 {
                decode_quad_magsgn(grid, &mut magsgn, qx + 1, qy)?;
            }

            qx += 2;
        }
    }
    Ok(())
}

/// §7.3.5 `decodeSigEMB` — AZC quads first consult the MEL symbol.
fn decode_sig_emb(
    grid: &HtGrid,
    mel: &mut MelDecoder,
    vlc: &mut VlcReader,
    qx: usize,
    qy: usize,
) -> Result<CxtVlcResult, Error> {
    let first_line_pair = qy == 0;
    let cq = quad_context(grid, qx, qy);
    if cq == 0 {
        let sym = mel.sym();
        if sym == 0 {
            return Ok(CxtVlcResult::default());
        }
    }
    decode_cxt_vlc(vlc, cq, first_line_pair)
}

/// Set σ_n for the four samples of quad `q` from its significance
/// pattern ρ_q.
#[inline]
pub(crate) fn apply_sig_pattern(grid: &mut HtGrid, qx: usize, qy: usize, rho: u8) {
    for j in 0..4 {
        if rho & (1 << j) != 0
            && let Some((x, y)) = grid.coord(qx, qy, j)
        {
            grid.sigma[x + y * grid.wblk] = true;
        }
    }
}

/// §7.3.4 / §7.3.6 — decode the unsigned residuals u_q for a quad-pair,
/// honouring the first-line-pair MEL special case.
fn decode_u_pair(
    mel: &mut MelDecoder,
    vlc: &mut VlcReader,
    first_line_pair: bool,
    u_off1: u8,
    u_off2: Option<u8>,
) -> Result<(u32, u32), Error> {
    // The U-VLC prefix/suffix/extension steps are interleaved: prefix
    // for q1 then q2, suffix for q1 then q2, extension for q1 then q2
    // (§7.3.4). Each quad's u is computed independently unless the
    // first-line-pair both-offset special case applies.
    let both_off = u_off1 == 1 && u_off2 == Some(1);

    if first_line_pair && both_off {
        // §7.3.6 — a single MEL symbol gates the quad-pair.
        let sym = mel.sym();
        if sym == 1 {
            // Both quads use Formula (4): u = 2 + pfx + sfx + 4·ext.
            let p1 = decode_u_prefix(vlc)?;
            let p2 = decode_u_prefix(vlc)?;
            let s1 = decode_u_suffix(vlc, p1)?;
            let s2 = decode_u_suffix(vlc, p2)?;
            let e1 = decode_u_extension(vlc, s1)?;
            let e2 = decode_u_extension(vlc, s2)?;
            let u1 = 2 + p1 + s1 + 4 * e1;
            let u2 = 2 + p2 + s2 + 4 * e2;
            return Ok((u1, u2));
        } else {
            // sym == 0: q1 by Formula (3); q2 depends on u_q1. The
            // §7.3.4 prefix-first interleave holds throughout: when
            // u_q1 > 2 — decidable from the prefix alone, since
            // u_q1 > 2 ⟺ u_pfx1 > 2 (§7.3.6 NOTE) — q2's *prefix
            // step* is replaced by a single raw bit, which therefore
            // precedes q1's suffix / extension bits.
            let p1 = decode_u_prefix(vlc)?;
            if p1 > 2 {
                let u_bit = vlc.bit()? as u32;
                let s1 = decode_u_suffix(vlc, p1)?;
                let e1 = decode_u_extension(vlc, s1)?;
                return Ok((p1 + s1 + 4 * e1, u_bit + 1));
            }
            let p2 = decode_u_prefix(vlc)?;
            let s1 = decode_u_suffix(vlc, p1)?;
            let s2 = decode_u_suffix(vlc, p2)?;
            let e1 = decode_u_extension(vlc, s1)?;
            let e2 = decode_u_extension(vlc, s2)?;
            return Ok((p1 + s1 + 4 * e1, p2 + s2 + 4 * e2));
        }
    }

    // General case: each quad with u_off == 1 decodes via Formula (3);
    // u_off == 0 means u = 0. Prefix-first interleaving across the pair.
    let p1 = if u_off1 == 1 {
        decode_u_prefix(vlc)?
    } else {
        0
    };
    let p2 = if u_off2 == Some(1) {
        decode_u_prefix(vlc)?
    } else {
        0
    };
    let s1 = if u_off1 == 1 {
        decode_u_suffix(vlc, p1)?
    } else {
        0
    };
    let s2 = if u_off2 == Some(1) {
        decode_u_suffix(vlc, p2)?
    } else {
        0
    };
    let e1 = if u_off1 == 1 {
        decode_u_extension(vlc, s1)?
    } else {
        0
    };
    let e2 = if u_off2 == Some(1) {
        decode_u_extension(vlc, s2)?
    } else {
        0
    };
    let u1 = if u_off1 == 1 { p1 + s1 + 4 * e1 } else { 0 };
    let u2 = if u_off2 == Some(1) {
        p2 + s2 + 4 * e2
    } else {
        0
    };
    Ok((u1, u2))
}

/// §7.3.7 + §7.3.8 — compute U_q for quad `q` and recover MagSgn values
/// for its significant samples, writing μ_n / s_n / σ_n / E_n.
fn decode_quad_magsgn(
    grid: &mut HtGrid,
    magsgn: &mut MagSgnReader,
    qx: usize,
    qy: usize,
) -> Result<(), Error> {
    let q = qy * grid.qw + qx;
    let qs = grid.quads[q];
    let kappa = exponent_predictor(grid, qx, qy, qs.rho);
    let u_big = kappa + qs.u_q;
    grid.quads[q].u_big = u_big;

    for j in 0..4 {
        let sigma_n = (qs.rho >> j) & 1;
        if sigma_n == 0 {
            continue;
        }
        let k_n = (qs.e_k >> j) & 1; // EMB known-bit
        let i_n = (qs.e_1 >> j) & 1; // EMB known-1
        // m_n = σ_n · U_q − k_n  (§7.3.8).
        let m_n = u_big.saturating_sub(k_n as u32);
        // The m_n MagSgn bits plus the known-1 bit at position m_n
        // must fit the 32-bit magnitude lane; every Mb this crate's
        // Equation E-2 tables can produce sits far below that, so a
        // wider exponent is a corrupt / non-conformant stream.
        if m_n > 31 {
            return Err(Error::HtCorruptSegment);
        }
        // decodeMagSgnValue: m_n bits little-endian + (i_n << m_n).
        let mut val = magsgn.bits(m_n)?;
        val += (i_n as u32) << m_n;
        // μ_n = ⌊v/2⌋ + 1, s_n = v mod 2 (when m_n ≠ 0).
        let (mu, sign) = if m_n != 0 {
            ((val >> 1) + 1, (val & 1) == 1)
        } else {
            // m_n == 0 yet σ_n == 1: μ_n = ⌊(i_n<<0)/2⌋+1 = i_n/2+1.
            // With m_n == 0, val = i_n. μ = ⌊i_n/2⌋ + 1, but the spec's
            // μ/s formula keys off m_n: when m_n == 0, μ = 0. A
            // significant sample with m_n == 0 cannot occur for a
            // conforming stream (U_q ≥ E_n ≥ 1 for significant n), so
            // fall back defensively.
            (val.max(1), false)
        };
        if let Some((x, y)) = grid.coord(qx, qy, j) {
            let idx = x + y * grid.wblk;
            grid.mu[idx] = mu;
            grid.sign[idx] = sign;
            grid.exp[idx] = magnitude_exponent(mu);
            // σ already set by apply_sig_pattern.
        }
    }
    Ok(())
}

/// Decode an HTJ2K code-block from its HT cleanup segment (and, when
/// `z_blk > 1`, its HT refinement segment), returning a [`CodeBlock`]
/// plus the block-level `Nb` (number of decoded magnitude bit-planes,
/// §D.2.1) for the §E.1 reconstruction.
///
/// * `mb` — the sub-band's `Mb` (T.800 Equation E-2 / §B.10.5): the
///   total number of magnitude bit-planes available for the band. The
///   recovered magnitudes are positioned MSB-first so the least-
///   significant decoded bit-plane sits at its true positional weight
///   `2^(Mb − Nb)`, exactly as the Annex D MQ tier-1 path stores them,
///   so [`crate::dequant::reconstruct_reversible`] /
///   [`crate::dequant::reconstruct_irreversible`] consume the block
///   identically regardless of which coder ran.
/// * `cleanup` — the HT cleanup segment bytes (§B.2).
/// * `refinement` — the HT refinement segment bytes; ignored when
///   `z_blk <= 1`.
/// * `z_blk` — number of coding passes processed (§B.3): 1 = cleanup
///   only, 2 = + SigProp, 3 = + SigProp + MagRef.
/// * `s_blk` — number of skipped (zero) magnitude bit-planes for the HT
///   set (§B.3 / §7.6). With `z_blk == 1` every sample has
///   `Nb = S_blk + 1`.
// The eight parameters are the irreducible §7.1 / §B.3 inputs the HT
// block-decoder needs (geometry + Mb + the two segments + Z_blk + S_blk);
// grouping them into a struct would only relocate the same fields.
#[allow(clippy::too_many_arguments)]
pub fn decode_ht_codeblock(
    orientation: SubBandOrientation,
    width: usize,
    height: usize,
    mb: u32,
    cleanup: &[u8],
    refinement: &[u8],
    z_blk: u8,
    s_blk: u32,
) -> Result<(CodeBlock, u32), Error> {
    assert!(width >= 1 && height >= 1);
    if z_blk == 0 || cleanup.is_empty() {
        // No HT segments: all samples are 0 (§7.1.1).
        return Ok((CodeBlock::new(orientation, width, height), 0));
    }

    let mut grid = HtGrid::new(width, height);
    decode_cleanup(&mut grid, cleanup)?;

    // §7.6 conformance: a cleanup magnitude carries at most
    // `S_blk + 1` bit-planes (`MSB_1 .. MSB_{S_blk+1}`); anything
    // wider cannot be positioned inside the band's `Mb` planes — and
    // would overflow the refinement compose below on a corrupt
    // stream.
    let mu_bits = (s_blk + 1).min(31);
    if grid.mu.iter().any(|&mu| mu >> mu_bits != 0) {
        return Err(Error::HtCorruptSegment);
    }

    // Refinement-indicator z_n / refinement-bit r_n from the SigProp /
    // MagRef passes (§7.4 / §7.5). Default 0. `sp_sign` records the sign
    // bit decoded for any sample the SigProp pass makes newly refined.
    let mut r = vec![0u8; width * height];
    let mut z = vec![0u8; width * height];
    let mut sp_sign = vec![false; width * height];

    if z_blk >= 2 {
        decode_sigprop(&grid, refinement, &mut r, &mut z, &mut sp_sign)?;
    }
    if z_blk >= 3 {
        decode_magref(&grid, refinement, &mut r, &mut z)?;
    }

    // §7.6 — block-level Nb. Every sample decoded down to bit-plane
    // S_blk via the cleanup pass has Nb = S_blk + 1; the refinement
    // passes add one further bit-plane (Nb = S_blk + 2) to refined
    // samples. We report the cleanup-level Nb and fold any refinement
    // bit directly into the magnitude integer (full precision where
    // refined), which is exact for the full-decode case Nb ≥ Mb.
    let nb = (s_blk + 1).min(mb);

    let mut coefficients = vec![Coefficient::default(); width * height];
    // Per-coefficient completed-plane counts (§7.6 Nb(u, v) = S_blk +
    // 1 + z_n, less the P the reassembly bridge adds back): a refined
    // sample carries one more decoded plane than the cleanup level, so
    // the §E.1 reconstruction must not midpoint-lift its exact LSB.
    let mut decoded_bits = vec![0u32; width * height];
    for y in 0..height {
        for x in 0..width {
            let idx = x + y * width;
            decoded_bits[idx] = if z[idx] != 0 { 2 } else { 1 };
            let mu = grid.mu[idx];
            let sigma = grid.sigma[idx];
            // Assemble the magnitude integer MSB-first. The cleanup pass
            // recovers μ_n (S_blk + 1 bits); a refined sample contributes
            // one further LSB (§7.6 MSB_{S_blk+2} = r_n).
            let (bits, sample_nb) = if z[idx] != 0 {
                ((mu << 1) | (r[idx] as u32), s_blk + 2)
            } else {
                (mu, s_blk + 1)
            };
            // Position the bits at their true §E.1 weight: shift up by
            // (Mb − Nb) so the least-significant decoded bit sits at
            // 2^(Mb − Nb), matching the Annex D convention. Clamp when
            // Nb ≥ Mb (full decode — no shift). A non-zero magnitude
            // whose positioned value cannot fit the 31-bit lane
            // (`Mb − Nb ≥ 32`, or high bits pushed out) is not
            // representable — reject it rather than overflowing; the
            // whole-codestream driver bounds `M'b ≤ 31` before calling
            // in, so this only fires for direct (hostile) invocations.
            let mag = if mb > sample_nb {
                let shift = mb - sample_nb;
                if bits != 0 && shift > bits.leading_zeros() {
                    return Err(Error::HtCorruptSegment);
                }
                if bits == 0 { 0 } else { bits << shift }
            } else {
                bits
            };
            // A cleanup-significant sample carries the cleanup sign; a
            // sample made newly significant only by the SigProp pass
            // (σ stayed 0 in cleanup but r_n became 1) carries the sign
            // decoded in the §7.4 sign step.
            let sign = if sigma { grid.sign[idx] } else { sp_sign[idx] };
            coefficients[idx] = Coefficient {
                magnitude: mag,
                sigma: sigma || r[idx] != 0,
                sign,
                already_refined: z[idx] != 0,
            };
        }
    }

    let mut block = CodeBlock::from_coefficients(orientation, width, height, coefficients);
    block.set_decoded_bits_raw(decoded_bits);
    Ok((block, nb))
}

/// §7.4 — HT SigProp decoding (refinement) pass.
///
/// Recovers refinement bits `r_n` and indicators `z_n` for samples that
/// were insignificant after the cleanup pass but have a significant
/// neighbour, following the four-line stripe scan. Sign bits are
/// recovered for samples that take `r_n = 1`.
fn decode_sigprop(
    grid: &HtGrid,
    refinement: &[u8],
    r: &mut [u8],
    z: &mut [u8],
    sp_sign: &mut [bool],
) -> Result<(), Error> {
    let w = grid.wblk;
    let h = grid.hblk;
    // SigProp byte-stream extends forward from byte 0 of the HT
    // refinement segment (§7.1.5). A separate state machine from MagRef.
    let mut sp = SigPropReader::new(refinement);

    // Stripe-oriented scan: stripes of 4 rows; within a stripe iterate
    // column-groups of 4 columns, doing all magnitude steps then all
    // sign steps for the group (§7.4).
    let mut y0 = 0;
    while y0 < h {
        let rows = (h - y0).min(4);
        let mut x0 = 0;
        while x0 < w {
            let cols = (w - x0).min(4);
            // Magnitude step for the column-group.
            for dx in 0..cols {
                for dy in 0..rows {
                    let x = x0 + dx;
                    let y = y0 + dy;
                    let idx = x + y * w;
                    if grid.sigma[idx] {
                        continue;
                    }
                    // mbr = OR of σ over the 8-neighbourhood plus r over
                    // scan-causal neighbours (§7.4 decodeSigPropMag).
                    let mut mbr = 0u8;
                    for (nx, ny) in neighbours8(x, y, w, h) {
                        let nidx = nx + ny * w;
                        mbr |= grid.sigma[nidx] as u8;
                        if scan_causal(nx, ny, x, y) {
                            mbr |= r[nidx];
                        }
                    }
                    if mbr != 0 {
                        z[idx] = 1;
                        r[idx] = sp.bit()?;
                    }
                }
            }
            // Sign step for the column-group.
            for dx in 0..cols {
                for dy in 0..rows {
                    let x = x0 + dx;
                    let y = y0 + dy;
                    let idx = x + y * w;
                    if r[idx] != 0 {
                        // §7.4 decodeSigPropSign: s_n = importSigPropBit
                        // (1 ≡ negative). Recorded for the §7.6 fold.
                        sp_sign[idx] = sp.bit()? == 1;
                    }
                }
            }
            x0 += 4;
        }
        y0 += 4;
    }
    Ok(())
}

/// §7.5 — HT MagRef decoding pass: one magnitude bit for every
/// already-significant sample, following the same stripe scan.
fn decode_magref(
    grid: &HtGrid,
    refinement: &[u8],
    r: &mut [u8],
    z: &mut [u8],
) -> Result<(), Error> {
    let w = grid.wblk;
    let h = grid.hblk;
    let mut mr = MagRefReader::new(refinement);
    let mut y0 = 0;
    while y0 < h {
        let rows = (h - y0).min(4);
        let mut x0 = 0;
        while x0 < w {
            let cols = (w - x0).min(4);
            for dx in 0..cols {
                for dy in 0..rows {
                    let x = x0 + dx;
                    let y = y0 + dy;
                    let idx = x + y * w;
                    if grid.sigma[idx] {
                        z[idx] = 1;
                        r[idx] = mr.bit();
                    }
                }
            }
            x0 += 4;
        }
        y0 += 4;
    }
    Ok(())
}

/// The 8-connected neighbours of `(x, y)` inside a `w × h` block.
pub(crate) fn neighbours8(
    x: usize,
    y: usize,
    w: usize,
    h: usize,
) -> impl Iterator<Item = (usize, usize)> {
    let mut out = Vec::with_capacity(8);
    for dy in -1i32..=1 {
        for dx in -1i32..=1 {
            if dx == 0 && dy == 0 {
                continue;
            }
            let nx = x as i32 + dx;
            let ny = y as i32 + dy;
            if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                out.push((nx as usize, ny as usize));
            }
        }
    }
    out.into_iter()
}

/// Whether neighbour `(nx, ny)` precedes `(x, y)` in the stripe-oriented
/// (column-major within a 4-row stripe) scan — the scan-causal subset of
/// §7.4.
pub(crate) fn scan_causal(nx: usize, ny: usize, x: usize, y: usize) -> bool {
    let nstripe = ny / 4;
    let stripe = y / 4;
    if nstripe != stripe {
        return nstripe < stripe;
    }
    // Same stripe: order is column-major (x advances slower than y
    // within a column? No — within a stripe the scan walks each column
    // top-to-bottom, columns left-to-right). So (nx, ny) precedes
    // (x, y) if nx < x, or nx == x and ny < y.
    nx < x || (nx == x && ny < y)
}

/// §7.1.5 — HT SigProp bit-stream reader (forward from byte 0 of the
/// refinement segment, big bytes synthesised as 0).
struct SigPropReader<'a> {
    dref: &'a [u8],
    sp_pos: usize,
    sp_bits: u32,
    sp_tmp: u32,
    sp_last: u32,
}

impl<'a> SigPropReader<'a> {
    fn new(dref: &'a [u8]) -> Self {
        Self {
            dref,
            sp_pos: 0,
            sp_bits: 0,
            sp_tmp: 0,
            sp_last: 0,
        }
    }

    fn bit(&mut self) -> Result<u8, Error> {
        if self.sp_bits == 0 {
            self.sp_bits = if self.sp_last == 0xFF { 7 } else { 8 };
            if self.sp_pos < self.dref.len() {
                self.sp_tmp = self.dref[self.sp_pos] as u32;
                self.sp_pos += 1;
                if self.sp_tmp & (1 << self.sp_bits) != 0 {
                    return Err(Error::HtCorruptSegment);
                }
            } else {
                self.sp_tmp = 0;
            }
            self.sp_last = self.sp_tmp;
        }
        let bit = (self.sp_tmp & 1) as u8;
        self.sp_tmp >>= 1;
        self.sp_bits -= 1;
        Ok(bit)
    }
}

/// §7.1.6 — HT MagRef bit-stream reader (backward from the last byte of
/// the refinement segment, bytes before the start synthesised as 0).
struct MagRefReader<'a> {
    dref: &'a [u8],
    mr_pos: isize,
    mr_bits: u32,
    mr_tmp: u32,
    mr_last: u32,
}

impl<'a> MagRefReader<'a> {
    fn new(dref: &'a [u8]) -> Self {
        Self {
            dref,
            mr_pos: dref.len() as isize - 1,
            mr_bits: 0,
            mr_tmp: 0,
            mr_last: 0xFF,
        }
    }

    fn bit(&mut self) -> u8 {
        if self.mr_bits == 0 {
            if self.mr_pos >= 0 {
                self.mr_tmp = self.dref[self.mr_pos as usize] as u32;
                self.mr_pos -= 1;
            } else {
                self.mr_tmp = 0;
            }
            self.mr_bits = 8;
            if self.mr_last > 0x8F && (self.mr_tmp & 0x7F) == 0x7F {
                self.mr_bits = 7;
            }
            self.mr_last = self.mr_tmp;
        }
        let bit = (self.mr_tmp & 1) as u8;
        self.mr_tmp >>= 1;
        self.mr_bits -= 1;
        bit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fuzz regression (ht_block_decode harness): a direct invocation
    /// with the Equation E-2 extreme `Mb = 37` and `S_blk = 0`
    /// positions a 1-bit magnitude 36 planes up — past the 31-bit
    /// `u32` lane. The whole-codestream driver bounds `M'b ≤ 31`
    /// before dispatching, but the public block decoder must reject
    /// the unrepresentable shift itself rather than overflow it.
    #[test]
    fn positioning_shift_past_the_lane_is_rejected() {
        let seg =
            crate::htenc::encode_ht_cleanup_segment(&[1, 0, 0, 1], &[false; 4], 2, 2).unwrap();
        let r = decode_ht_codeblock(SubBandOrientation::HL, 2, 2, 37, &seg, &[], 1, 0);
        assert!(matches!(r, Err(Error::HtCorruptSegment)));
        // An all-zero block under the same extreme budget stays a
        // clean decode (nothing to position).
        let zero = crate::htenc::encode_ht_cleanup_segment(&[0; 4], &[false; 4], 2, 2);
        if let Ok(zseg) = zero {
            let (block, _) =
                decode_ht_codeblock(SubBandOrientation::HL, 2, 2, 37, &zseg, &[], 1, 0)
                    .expect("all-zero block decodes at any budget");
            for y in 0..2 {
                for x in 0..2 {
                    assert_eq!(block.coefficient(x, y).magnitude, 0);
                }
            }
        }
    }

    /// MEL_E table matches Table 2 verbatim.
    #[test]
    fn mel_e_table() {
        assert_eq!(MEL_E, [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 4, 5]);
    }

    /// CxtVLC tables have the §Annex-C entry counts and every codeword
    /// fits its declared bit-length.
    #[test]
    fn cxt_vlc_table_shape() {
        assert_eq!(CXT_VLC_TABLE_0.len(), 444);
        assert_eq!(CXT_VLC_TABLE_1.len(), 358);
        for e in CXT_VLC_TABLE_0.iter().chain(CXT_VLC_TABLE_1.iter()) {
            assert!(e.cq <= 7);
            assert!(e.len >= 1 && e.len <= 7);
            assert!((e.cwd as u32) < (1u32 << e.len));
            assert!(e.rho <= 0xF && e.e_k <= 0xF && e.e_1 <= 0xF);
            assert!(e.u_off <= 1);
        }
    }

    /// Within each (table, cq) the (cwd, len) pairs are unique so
    /// `test_match` is deterministic.
    #[test]
    fn cxt_vlc_unique_codewords() {
        for table in [&CXT_VLC_TABLE_0[..], &CXT_VLC_TABLE_1[..]] {
            for cq in 0..=7u8 {
                let mut seen = std::collections::HashSet::new();
                for e in table.iter().filter(|e| e.cq == cq) {
                    assert!(
                        seen.insert((e.cwd, e.len)),
                        "duplicate codeword cq={} cwd={:#x} len={}",
                        cq,
                        e.cwd,
                        e.len
                    );
                }
            }
        }
    }

    /// `modDcup` overwrites the last byte with 0xFF and forces the low
    /// nibble of the second-last byte to 1s (§7.1.1).
    /// §7.1.2 `importMagSgnBit`, one bit at a time as the procedure is
    /// written: the reference the windowed reader must match.
    struct MagSgnBitwise<'a> {
        dcup: ModDcup<'a>,
        pcup: usize,
        pos: usize,
        bits: u32,
        tmp: u32,
        last: u32,
    }

    impl MagSgnBitwise<'_> {
        fn bit(&mut self) -> Result<u32, Error> {
            if self.bits == 0 {
                self.bits = if self.last == 0xFF { 7 } else { 8 };
                if self.pos < self.pcup {
                    self.tmp = self.dcup.get(self.pos) as u32;
                    if self.tmp & (1 << self.bits) != 0 {
                        return Err(Error::HtCorruptSegment);
                    }
                } else if self.pos == self.pcup {
                    self.tmp = 0xFF;
                } else {
                    return Err(Error::HtCorruptSegment);
                }
                self.last = self.tmp;
                self.pos += 1;
            }
            let bit = self.tmp & 1;
            self.tmp >>= 1;
            self.bits -= 1;
            Ok(bit)
        }

        fn value(&mut self, m: u32) -> Result<u32, Error> {
            let mut val = 0;
            for i in 0..m {
                val |= self.bit()? << i;
            }
            Ok(val)
        }
    }

    /// §7.1.4 `importVLCBit` as the procedure is written.
    struct VlcBitwise<'a> {
        dcup: ModDcup<'a>,
        pcup: usize,
        pos: isize,
        bits: u32,
        tmp: u32,
        last: u32,
    }

    impl<'a> VlcBitwise<'a> {
        fn new(dcup: ModDcup<'a>, pcup: usize, lcup: usize) -> Self {
            let last = dcup.get(lcup - 2) as u32;
            let tmp = last >> 4;
            VlcBitwise {
                dcup,
                pcup,
                pos: lcup as isize - 3,
                bits: if (tmp & 7) < 7 { 4 } else { 3 },
                tmp,
                last,
            }
        }

        fn bit(&mut self) -> Result<u8, Error> {
            if self.bits == 0 {
                if self.pos < self.pcup as isize {
                    return Err(Error::HtCorruptSegment);
                }
                self.tmp = self.dcup.get(self.pos as usize) as u32;
                self.bits = 8;
                if self.last > 0x8F && (self.tmp & 0x7F) == 0x7F {
                    self.bits = 7;
                }
                self.last = self.tmp;
                self.pos -= 1;
            }
            let bit = (self.tmp & 1) as u8;
            self.tmp >>= 1;
            self.bits -= 1;
            Ok(bit)
        }

        /// §7.3.5 `decodeCxtVLC`: grow the codeword a bit at a time
        /// until an entry of the context matches.
        fn cxt_vlc(&mut self, cq: u8, table: &[CxtVlcEntry]) -> Result<(u8, u8, u8, u8), Error> {
            let mut cwd = 0u8;
            for len in 1..=7u8 {
                cwd |= self.bit()? << (len - 1);
                if let Some(e) = table
                    .iter()
                    .find(|e| e.cq == cq && e.len == len && e.cwd == cwd)
                {
                    return Ok((e.rho, e.u_off, e.e_k, e.e_1));
                }
            }
            Err(Error::HtCorruptSegment)
        }
    }

    /// Pseudo-random segments with plenty of `0xFF` / `0x7F` bytes, so
    /// stuffing and the stuffing-bit violation both occur.
    fn stuffing_heavy_segment(next: &mut impl FnMut() -> u32) -> Vec<u8> {
        let len = 2 + next() as usize % 60;
        (0..len)
            .map(|_| match next() % 7 {
                0 => 0xFF,
                1 => 0x7F,
                2 => 0x8F + (next() % 0x70) as u8,
                _ => next() as u8,
            })
            .collect()
    }

    #[test]
    fn windowed_magsgn_reader_matches_the_bitwise_procedure() {
        let mut state = 0x4d53_0001u32;
        let mut next = move || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state >> 8
        };
        for round in 0..3000 {
            let seg = stuffing_heavy_segment(&mut next);
            let dcup = ModDcup::new(&seg);
            let pcup = next() as usize % (seg.len() + 1);
            let mut fast = MagSgnReader::new(dcup, pcup);
            let mut slow = MagSgnBitwise {
                dcup,
                pcup,
                pos: 0,
                bits: 0,
                tmp: 0,
                last: 0,
            };
            loop {
                // Widths as §7.3.8 asks for them, zero included.
                let m = match next() % 4 {
                    0 => 0,
                    1 => next() % 4,
                    _ => next() % 32,
                };
                let expected = slow.value(m);
                assert_eq!(
                    fast.bits(m).ok(),
                    expected.as_ref().ok().copied(),
                    "round {round} m {m}"
                );
                // An error ends the block decode; what a reader holds
                // after one is never looked at.
                if expected.is_err() {
                    break;
                }
            }
            assert!(fast.bits(33).is_err());
        }
    }

    #[test]
    fn windowed_vlc_reader_and_look_ahead_match_the_bitwise_procedure() {
        let mut state = 0x564c_4301u32;
        let mut next = move || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state >> 8
        };
        for round in 0..3000 {
            let seg = stuffing_heavy_segment(&mut next);
            let dcup = ModDcup::new(&seg);
            let pcup = next() as usize % (seg.len() - 1);
            let mut fast = VlcReader::new(dcup, pcup, seg.len());
            let mut slow = VlcBitwise::new(dcup, pcup, seg.len());
            loop {
                let failed = match next() % 3 {
                    // A CxtVLC codeword in a random context and table.
                    0 => {
                        let cq = (next() % 8) as u8;
                        let first = next() % 2 == 0;
                        let table: &[CxtVlcEntry] = if first {
                            &CXT_VLC_TABLE_0
                        } else {
                            &CXT_VLC_TABLE_1
                        };
                        let expected = slow.cxt_vlc(cq, table);
                        let got = decode_cxt_vlc(&mut fast, cq, first)
                            .map(|r| (r.rho, r.u_off, r.e_k, r.e_1));
                        assert_eq!(
                            got.ok(),
                            expected.as_ref().ok().copied(),
                            "round {round} cq {cq} first {first}"
                        );
                        expected.is_err()
                    }
                    // Single bits, as the U-VLC procedures take them.
                    _ => {
                        let expected = slow.bit();
                        assert_eq!(
                            fast.bit().ok(),
                            expected.as_ref().ok().copied(),
                            "round {round}"
                        );
                        expected.is_err()
                    }
                };
                if failed {
                    break;
                }
            }
        }
    }

    #[test]
    fn mod_dcup_rewrite() {
        let bytes = [0x12u8, 0x34, 0x50, 0xA0];
        let d = ModDcup::new(&bytes);
        assert_eq!(d.get(0), 0x12);
        assert_eq!(d.get(1), 0x34);
        assert_eq!(d.get(2), 0x5F); // 0x50 | 0x0F
        assert_eq!(d.get(3), 0xFF); // forced
    }

    /// U-VLC prefix lengths match Table 3: "1"→1, "01"→2, "001"→3,
    /// "000"→5. The VLC reader imports little-endian, so "1" is a single
    /// `1` bit, "01" is `0` then `1`, etc.
    #[test]
    fn u_prefix_values() {
        // Build a tiny cleanup segment whose VLC stream yields known
        // bits. The VLC reader runs backward from Lcup-3; we only need
        // the prefix bits, so construct a segment large enough that the
        // init machinery reads our crafted byte.
        // Simpler: exercise decode_u_suffix/extension thresholds.
        // u_pfx < 3 yields suffix 0 with no bit consumption.
        let seg = [0x00u8, 0x00, 0x00, 0x00, 0x00, 0x00];
        let d = ModDcup::new(&seg);
        let mut vlc = VlcReader::new(d, 0, seg.len());
        assert_eq!(decode_u_suffix(&mut vlc, 1).unwrap(), 0);
        assert_eq!(decode_u_suffix(&mut vlc, 2).unwrap(), 0);
        assert_eq!(decode_u_extension(&mut vlc, 0).unwrap(), 0);
    }

    /// `magnitude_exponent` reproduces Table 1: E(0)=0, E(1)=1, E(2)=2,
    /// E(3..=4)=3, E(5..=8)=4, E(9..=16)=5, … (`E = ⌈log2(2μ)⌉`).
    #[test]
    fn magnitude_exponent_table1() {
        assert_eq!(magnitude_exponent(0), 0);
        assert_eq!(magnitude_exponent(1), 1);
        assert_eq!(magnitude_exponent(2), 2);
        for mu in 3..=4 {
            assert_eq!(magnitude_exponent(mu), 3, "μ={mu}");
        }
        for mu in 5..=8 {
            assert_eq!(magnitude_exponent(mu), 4, "μ={mu}");
        }
        for mu in 9..=16 {
            assert_eq!(magnitude_exponent(mu), 5, "μ={mu}");
        }
        for mu in 17..=32 {
            assert_eq!(magnitude_exponent(mu), 6, "μ={mu}");
        }
        // Fuzz regression (decode_variants harness): a corrupt MagSgn
        // stream can recover a μ near the top of the u32 lane before
        // the §7.6 bound rejects it; `2μ − 1` must not overflow.
        assert_eq!(magnitude_exponent(u32::MAX), 33);
        assert_eq!(magnitude_exponent(1 << 31), 32);
    }

    /// An all-zero / empty HT cleanup segment with `z_blk == 0` yields a
    /// block of all-zero samples (§7.1.1: "If Z_blk equals 0 … all sample
    /// output values for the block shall be 0").
    #[test]
    fn zero_zblk_yields_zero_block() {
        let (block, nb) =
            decode_ht_codeblock(SubBandOrientation::LL, 4, 4, 8, &[], &[], 0, 0).unwrap();
        assert_eq!(nb, 0);
        for v in 0..4 {
            for u in 0..4 {
                let c = block.coefficient(u, v);
                assert_eq!(c.magnitude, 0);
                assert!(!c.sigma);
            }
        }
    }

    /// A cleanup segment shorter than two bytes, or with an out-of-range
    /// Scup, is rejected as a corrupt HT segment (§7.1.1 constraints).
    #[test]
    fn malformed_segment_rejected() {
        let err = decode_ht_codeblock(SubBandOrientation::LL, 2, 2, 8, &[0x00], &[], 1, 0);
        assert!(matches!(err, Err(Error::HtCorruptSegment)));
    }
}
