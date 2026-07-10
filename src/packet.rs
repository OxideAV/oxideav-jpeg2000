//! Tier-2 packet-header reading primitives — T.800 §B.10.
//!
//! This module implements the **structural** parts of the JPEG 2000
//! Part-1 packet-header coding described in T.800 §B.10. It is the
//! glue between the tile-part walker (round 2, [`crate::walk_tile_parts`])
//! and the tier-1 EBCOT block coder (queued for a later round). The
//! reader operates entirely in the bit-stuffed byte stream of a
//! tile-part body (`TilePart::body_offset .. body_offset + body_len`)
//! and consumes one packet header at a time, producing a typed
//! [`PacketHeader`] that lists, for each code-block referenced by the
//! caller-supplied geometry, whether the block contributes to this
//! packet, how many coding passes it includes, and the byte lengths
//! of those passes' codeword segments.
//!
//! ## What is structural and what is not
//!
//! "Structural" means we walk the bitstream and pull out the
//! signalling fields. We do **not** decode the tier-1 EBCOT
//! coefficients, do not perform the inverse wavelet transform, and do
//! not reassemble samples. The byte ranges of the per-code-block
//! codeword segments are returned to callers; the actual MQ-coder
//! decode of those bytes lives in a later round.
//!
//! ## Geometry input
//!
//! T.800 §B.12 specifies the progression order — the sequence of
//! `(layer, resolution, component, precinct)` tuples whose packets
//! appear in the codestream. Computing that sequence from the COD /
//! SIZ marker segments is its own substantial body of code (§B.6
//! divides resolution levels into precincts, §B.7 divides sub-bands
//! into code-blocks). Round 5 keeps the geometry computation **out
//! of scope** and instead accepts a caller-built
//! [`PacketGeometry`] slice describing each packet in order — one
//! entry per packet — listing the sub-bands present in that packet
//! (LL only for resolution 0, HL/LH/HH for r > 0) and, for each
//! sub-band, the number of code-blocks in raster order. That keeps
//! the round's surface a pure reader: feed bytes + geometry → get
//! `Vec<PacketHeader>` out. The geometry-from-COD computation lands
//! in round 6.
//!
//! ## References
//!
//! Built entirely from T.800 §B.10 (`docs/image/jpeg2000/t800.txt`
//! lines ~3766-4030 / pages 70-74 of the spec PDF):
//!
//! * §B.10.1 — Bit-stuffing routine.
//! * §B.10.2 — Tag trees + Figure B.12.
//! * §B.10.3 — Zero length packet (first bit of the header).
//! * §B.10.4 — Code-block inclusion (1 bit for re-included blocks,
//!   tag tree for first inclusion).
//! * §B.10.5 — Zero bit-plane information (tag tree).
//! * §B.10.6 — Number of coding passes + Table B.4.
//! * §B.10.7 — Codeword-segment length (`Lblock` mechanism, single
//!   and multiple codeword-segment cases).
//! * §B.10.8 — Order of information within a packet header.
//!

use crate::Error;

// ---------------------------------------------------------------------------
// Bit reader — T.800 §B.10.1 (bit-stuffing routine).
// ---------------------------------------------------------------------------

/// MSB-first bit reader implementing the T.800 §B.10.1 bit-stuffing
/// rule.
///
/// Per §B.10.1 the encoder packs bits MSB-first into bytes; once a
/// byte assembled equals `0xFF`, the encoder inserts an extra zero
/// bit at the MSB of the **next** byte. On the read side this means
/// that whenever the previous byte was `0xFF` we must skip one
/// (always-zero) bit at the top of the next byte before resuming the
/// 8-bit read pattern.
///
/// The last byte of a packet header is padded to the byte boundary
/// (§B.10.1 final paragraph) and shall not be `0xFF`. The reader
/// surfaces [`PacketBitReader::align_to_byte`] for callers that want
/// to skip those padding bits explicitly between packets.
#[derive(Debug)]
pub struct PacketBitReader<'a> {
    bytes: &'a [u8],
    /// Byte index of the next byte to consume (or, if `bits_left > 0`,
    /// the byte one past the byte currently being shifted out of
    /// `cur`).
    next_byte: usize,
    /// Number of bits remaining in `cur` (0..=8).
    bits_left: u8,
    /// Bits queued for delivery — left-aligned (next bit to deliver
    /// is bit `bits_left - 1` of `cur`).
    cur: u8,
    /// `true` iff the previous byte consumed from `bytes` was `0xFF`;
    /// the next byte therefore has a stuffed zero bit at its MSB
    /// that must be skipped before its remaining 7 bits are used.
    prev_was_ff: bool,
}

impl<'a> PacketBitReader<'a> {
    /// Wraps a byte slice (typically a tile-part body sub-range) for
    /// bit-stuffed reading.
    pub fn new(bytes: &'a [u8]) -> Self {
        PacketBitReader {
            bytes,
            next_byte: 0,
            bits_left: 0,
            cur: 0,
            prev_was_ff: false,
        }
    }

    /// Number of bytes that have been fully consumed from the input
    /// slice. Always points at the first unread byte; callers compare
    /// this against the input length to know when the slice is
    /// exhausted.
    pub fn bytes_consumed(&self) -> usize {
        // If we have bits left in `cur`, the byte that produced them
        // has been advanced past in `next_byte` already. The caller
        // reading bytes_consumed wants the number of FULLY consumed
        // bytes — i.e., the bytes whose every bit has been delivered.
        // When mid-byte, that's `next_byte - 1` (the byte we're
        // currently emitting from), so we return `next_byte` only when
        // aligned. Most callers will use `align_to_byte()` first.
        if self.bits_left == 0 {
            self.next_byte
        } else {
            self.next_byte - 1
        }
    }

    /// Reads a single bit. Returns `Err(Error::UnexpectedEof)` if
    /// the input is exhausted mid-byte.
    pub fn read_bit(&mut self) -> Result<u8, Error> {
        if self.bits_left == 0 {
            // Fetch the next byte.
            if self.next_byte >= self.bytes.len() {
                return Err(Error::UnexpectedEof);
            }
            let b = self.bytes[self.next_byte];
            self.next_byte += 1;
            if self.prev_was_ff {
                // §B.10.1: the stuffed bit is the MSB of this byte
                // (always 0). Strip it.
                self.cur = b << 1;
                self.bits_left = 7;
            } else {
                self.cur = b;
                self.bits_left = 8;
            }
            self.prev_was_ff = b == 0xFF;
        }
        // Deliver the top bit.
        let bit = (self.cur >> 7) & 0x01;
        self.cur <<= 1;
        self.bits_left -= 1;
        Ok(bit)
    }

    /// Reads `n` bits (1..=32) as a big-endian unsigned integer.
    /// The first bit read becomes the most significant bit of the
    /// result.
    pub fn read_bits(&mut self, n: u8) -> Result<u32, Error> {
        if n == 0 {
            return Ok(0);
        }
        if n > 32 {
            return Err(Error::InvalidPacketHeader);
        }
        let mut v: u32 = 0;
        for _ in 0..n {
            v = (v << 1) | (self.read_bit()? as u32);
        }
        Ok(v)
    }

    /// Advances the reader to the next byte boundary, discarding any
    /// bits queued in the current byte. T.800 §B.10.1: a packet header
    /// is padded to a whole number of bytes, so the bits between the
    /// last meaningful bit and the next byte boundary are discarded
    /// padding.
    pub fn align_to_byte(&mut self) {
        if self.bits_left != 0 {
            self.bits_left = 0;
            self.cur = 0;
        }
    }
}

// ---------------------------------------------------------------------------
// Tag tree — T.800 §B.10.2 / Figure B.12.
// ---------------------------------------------------------------------------

/// Tag-tree decoder per T.800 §B.10.2.
///
/// A tag tree represents a 2-D array of non-negative integers via a
/// hierarchical minimum encoding. At each node we record the minimum
/// of its (up to four) child values; the root is at level 0. The
/// decode procedure queries the tree along a path from the root to a
/// leaf, reading bits from the packet header bit stream and updating
/// each visited node's "current best lower bound" until the leaf's
/// actual value is known to equal or exceed a caller-supplied
/// threshold.
///
/// The tag tree carries state between successive `decode_value` calls
/// — partially-revealed nodes do not need to be re-queried on the
/// next access. This is the "causality" property called out by T.800
/// §B.10.2: "Only the information needed for the current code-block
/// is stored at the current point in the packet header [...] this
/// information is not coded again."
#[derive(Debug, Clone)]
pub struct TagTree {
    width: u32,
    height: u32,
    /// Per-level node grid; `levels[0]` is the lowest-resolution
    /// level (the root), `levels[depth-1]` is the leaf level. Each
    /// level stores `(value, fully_decoded)` per node in raster
    /// order.
    levels: Vec<Vec<(u32, bool)>>,
}

impl TagTree {
    /// Builds a new tag tree spanning a `width × height` leaf grid.
    /// All node values start at 0 and all `fully_decoded` flags at
    /// `false`, matching the per-node initial state defined by T.800
    /// §B.10.2 ("each node has an associated current value, which is
    /// initialised to zero").
    ///
    /// `width` or `height` of zero yields an empty tree whose
    /// `decode_value` always errors — callers should branch on that
    /// before invoking the tree.
    pub fn new(width: u32, height: u32) -> Self {
        if width == 0 || height == 0 {
            return TagTree {
                width,
                height,
                levels: Vec::new(),
            };
        }
        // Compute the per-level dimensions: level 0 is the root
        // (1×1 once we keep halving). T.800 §B.10.2 says the tree
        // "successively creates reduced resolution levels of this
        // two-dimensional array"; in practice that means each level
        // halves the previous level's dimensions (rounding up) until
        // the level is 1×1.
        let mut dims: Vec<(u32, u32)> = Vec::new();
        let mut w = width;
        let mut h = height;
        dims.push((w, h));
        while w > 1 || h > 1 {
            w = w.div_ceil(2);
            h = h.div_ceil(2);
            dims.push((w, h));
        }
        // dims has the leaves first (largest), root last (1×1). Build
        // levels[] root-first to match the "level 0 = root" naming
        // from §B.10.2.
        dims.reverse();
        let levels = dims
            .into_iter()
            .map(|(lw, lh)| vec![(0u32, false); (lw as usize) * (lh as usize)])
            .collect();
        TagTree {
            width,
            height,
            levels,
        }
    }

    /// Returns the leaf grid width × height.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Tree depth — number of levels from root (inclusive) to leaves
    /// (inclusive). A 1×1 tag tree has depth 1 (just the leaf).
    pub fn depth(&self) -> usize {
        self.levels.len()
    }

    /// Decodes whether the leaf at `(x, y)` is **strictly less than**
    /// `threshold`. This is the operation T.800 §B.10.4 / §B.10.5
    /// invoke when asking "is this code-block included by layer
    /// `threshold`?" / "is this code-block's number-of-missing-MSBs
    /// less than `threshold`?".
    ///
    /// The leaf value is updated in-place as the read progresses, so
    /// subsequent queries on the same `(x, y)` or sibling leaves do
    /// not re-read bits the spec already committed to the stream.
    ///
    /// Returns `Ok(true)` iff the leaf's value is `< threshold` after
    /// reading just enough bits to decide. Returns `Ok(false)` iff
    /// the read confirmed `value >= threshold` without revealing the
    /// exact value (the partial-tag-tree case from §B.10.4).
    pub fn decode_below_threshold(
        &mut self,
        x: u32,
        y: u32,
        threshold: u32,
        reader: &mut PacketBitReader<'_>,
    ) -> Result<bool, Error> {
        if self.levels.is_empty() || x >= self.width || y >= self.height {
            return Err(Error::InvalidPacketHeader);
        }
        let depth = self.levels.len();
        // Walk from root (level 0) down to the leaf at (x, y).
        // The (x, y) coordinates at level `i` from the leaf side are
        // (x >> (leaf_levels_below), y >> (leaf_levels_below)) where
        // leaf_levels_below = depth - 1 - i.
        let mut value_above: u32 = 0;
        for level in 0..depth {
            let shift = (depth - 1 - level) as u32;
            let lx = x >> shift;
            let ly = y >> shift;
            let lw = self.level_width(level);
            let node_idx = (ly as usize) * (lw as usize) + (lx as usize);
            // The node's lower bound starts at max(its current value,
            // value_above), per §B.10.2 (a parent's minimum is a lower
            // bound for every child's actual value).
            let (mut v, mut decoded) = self.levels[level][node_idx];
            if v < value_above {
                v = value_above;
            }
            // Read 0 bits while the lower bound is still < threshold.
            while !decoded && v < threshold {
                let bit = reader.read_bit()?;
                if bit == 0 {
                    v += 1;
                } else {
                    decoded = true;
                }
            }
            // Persist back.
            self.levels[level][node_idx] = (v, decoded);
            if !decoded {
                // We stopped because v >= threshold without revealing
                // the exact value. The leaf is at-or-above threshold.
                return Ok(false);
            }
            // Decoded → this node's value is committed at `v` and
            // serves as the lower bound for the next level.
            value_above = v;
        }
        // Reached the leaf, fully decoded. value_above holds the leaf
        // value. Compare against threshold.
        Ok(value_above < threshold)
    }

    /// Decodes the exact value at a leaf, reading just enough bits to
    /// commit the leaf's value. Equivalent to repeatedly calling
    /// [`Self::decode_below_threshold`] with thresholds 1, 2, …
    /// until it returns `true` and reporting the count of
    /// `false`-returning calls — but does so in a single bit-read pass.
    ///
    /// Used by §B.10.5 (zero bit-plane information): the
    /// number-of-missing-MSBs P for a code-block on first inclusion is
    /// read as the full leaf value.
    pub fn decode_value(
        &mut self,
        x: u32,
        y: u32,
        reader: &mut PacketBitReader<'_>,
    ) -> Result<u32, Error> {
        if self.levels.is_empty() || x >= self.width || y >= self.height {
            return Err(Error::InvalidPacketHeader);
        }
        let depth = self.levels.len();
        let mut value_above: u32 = 0;
        for level in 0..depth {
            let shift = (depth - 1 - level) as u32;
            let lx = x >> shift;
            let ly = y >> shift;
            let lw = self.level_width(level);
            let node_idx = (ly as usize) * (lw as usize) + (lx as usize);
            let (mut v, mut decoded) = self.levels[level][node_idx];
            if v < value_above {
                v = value_above;
            }
            // Read until a 1 bit commits the value at this level.
            while !decoded {
                let bit = reader.read_bit()?;
                if bit == 0 {
                    v += 1;
                } else {
                    decoded = true;
                }
            }
            self.levels[level][node_idx] = (v, decoded);
            value_above = v;
        }
        Ok(value_above)
    }

    fn level_width(&self, level: usize) -> u32 {
        // levels[0] is root (smallest). The level's width is the leaf
        // width right-shifted by (depth - 1 - level), rounded up.
        let depth = self.levels.len();
        let shift = (depth - 1 - level) as u32;
        let mut w = self.width;
        for _ in 0..shift {
            w = w.div_ceil(2);
        }
        w
    }
}

// ---------------------------------------------------------------------------
// Coding-passes Huffman — T.800 §B.10.6 / Table B.4.
// ---------------------------------------------------------------------------

/// Decodes the number of coding passes from a code-block's
/// contribution to one packet per T.800 §B.10.6 / Table B.4.
///
/// The codeword space:
///
/// * `0` (1 bit) → 1 coding pass.
/// * `10` (2 bits) → 2.
/// * `1100`, `1101`, `1110` (4 bits) → 3, 4, 5 respectively.
/// * Otherwise read four more bits A (so prefix `1111` plus A as
///   5-bit suffix, 9 bits total):
///   * If A in 0..30: value = 6 + A (range 6..36).
///   * If A == 31 (`11111`): read 7 more bits B (16 bits total),
///     value = 37 + B (range 37..164).
pub fn decode_coding_passes(reader: &mut PacketBitReader<'_>) -> Result<u32, Error> {
    let b0 = reader.read_bit()?;
    if b0 == 0 {
        return Ok(1);
    }
    let b1 = reader.read_bit()?;
    if b1 == 0 {
        return Ok(2);
    }
    // After `11`, read 2 more bits.
    let b2 = reader.read_bit()?;
    let b3 = reader.read_bit()?;
    let lo2 = (b2 << 1) | b3;
    if lo2 == 0b00 {
        return Ok(3);
    }
    if lo2 == 0b01 {
        return Ok(4);
    }
    if lo2 == 0b10 {
        return Ok(5);
    }
    // lo2 == 0b11 — prefix `1111` consumed; escape into the 9-bit
    // range (6..36) or further into the 16-bit range (37..164).
    let a = reader.read_bits(5)?;
    if a < 31 {
        Ok(6 + a)
    } else {
        // a == 31 → escape into the 16-bit range.
        let b = reader.read_bits(7)?;
        Ok(37 + b)
    }
}

// ---------------------------------------------------------------------------
// Codeword-segment length — T.800 §B.10.7.1.
// ---------------------------------------------------------------------------

/// State carried per code-block across successive packets — T.800
/// §B.10.7.1 says `Lblock` is initially 3 and may only ever increase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LblockState {
    /// Current `Lblock` for this code-block (initial: 3).
    pub lblock: u32,
}

impl Default for LblockState {
    fn default() -> Self {
        LblockState { lblock: 3 }
    }
}

/// Reads the leading "increase Lblock by k" prefix from the bit
/// stream — k ones followed by a terminating zero. Returns the
/// updated `Lblock`.
///
/// T.800 §B.10.7.1: "A signalling bit of zero indicates the current
/// value of Lblock is sufficient. If there are k ones followed by a
/// zero, the value of Lblock is incremented by k."
fn read_lblock_increment(
    state: &mut LblockState,
    reader: &mut PacketBitReader<'_>,
) -> Result<(), Error> {
    let mut k: u32 = 0;
    loop {
        let bit = reader.read_bit()?;
        if bit == 0 {
            break;
        }
        k += 1;
        if k > 32 {
            // §B.10.7.1 doesn't impose an explicit cap, but practical
            // Lblock values stay small. Catch runaway streams.
            return Err(Error::InvalidPacketHeader);
        }
    }
    state.lblock = state
        .lblock
        .checked_add(k)
        .ok_or(Error::InvalidPacketHeader)?;
    Ok(())
}

/// Decodes a single codeword-segment length using the `Lblock`
/// mechanism per T.800 §B.10.7.1. `passes_in_segment` is the number
/// of coding passes contributed to this segment (1 for the
/// single-codeword case; the per-segment K-fold split is described
/// in §B.10.7.2).
///
/// The signalling-bit prefix at the start of the segment encodes a
/// fresh `Lblock` increment (zero bits = "current Lblock is enough",
/// k ones plus terminating zero = "increase Lblock by k"). Then the
/// length itself is read as a `(Lblock + floor(log2 passes_in_segment))`-bit
/// big-endian unsigned integer.
pub fn decode_segment_length(
    state: &mut LblockState,
    passes_in_segment: u32,
    reader: &mut PacketBitReader<'_>,
) -> Result<u32, Error> {
    read_lblock_increment(state, reader)?;
    read_segment_length_value(state.lblock, passes_in_segment, reader)
}

/// Reads one §B.10.7.1 codeword-segment length **value** from the bit
/// stream, given an already-resolved `lblock` (no leading
/// increase-Lblock prefix is consumed). The field is
/// `(lblock + floor(log2 passes_in_segment))` bits, big-endian.
///
/// This is the inner half of [`decode_segment_length`]. It is also the
/// per-length read for the §B.10.7.2 multiple-codeword-segment case,
/// where the increase-Lblock prefix is signalled **only once** before
/// the first length (per the §B.10.7.2 worked example: "the value of
/// Lblock is incremented only at the start of the sequence") and the
/// remaining `K − 1` lengths are read directly with the same `Lblock`.
fn read_segment_length_value(
    lblock: u32,
    passes_in_segment: u32,
    reader: &mut PacketBitReader<'_>,
) -> Result<u32, Error> {
    let bits = segment_length_width(lblock, passes_in_segment)?;
    reader.read_bits(bits)
}

/// The §B.10.7.1 / Equation B-19 bit width of one codeword-segment
/// length field: `lblock + floor(log2 passes_in_segment)`, validated
/// against the bit reader's 32-bit ceiling.
fn segment_length_width(lblock: u32, passes_in_segment: u32) -> Result<u8, Error> {
    let extra = if passes_in_segment <= 1 {
        0
    } else {
        // floor(log2(passes_in_segment)) — passes_in_segment is at
        // most 164 (§B.10.6 NOTE caps the practical maximum), so the
        // value fits comfortably in a u32 ilog2.
        passes_in_segment.ilog2()
    };
    let bits = lblock
        .checked_add(extra)
        .ok_or(Error::InvalidPacketHeader)?;
    if bits == 0 || bits > 32 {
        // Lblock starts at 3 and only grows; a zero-bit read would be
        // a malformed input. Cap at 32 to match the bit reader's
        // own read_bits ceiling.
        return Err(Error::InvalidPacketHeader);
    }
    Ok(bits as u8)
}

/// The kind of coding pass at absolute pass index `i` within a
/// code-block, per the T.800 §D.3 schedule. Index 0 is the first
/// (cleanup-only) pass on the code-block's first non-empty bit-plane;
/// from index 1 onward the passes run in repeating
/// significance-propagation → magnitude-refinement → cleanup triples.
///
/// `0` = significance propagation, `1` = magnitude refinement,
/// `2` = cleanup. Returned as a small tag so the §B.10.7.2 / Table D.9
/// terminated-pass classification can be computed without pulling in
/// the [`crate::t1::Pass`] type (this module is below tier-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PassRole {
    Sp,
    Mr,
    Cleanup,
}

/// Pass role at absolute pass index `i` (§D.3): index 0 is cleanup,
/// then `(i − 1) mod 3` selects SP / MR / cleanup.
fn pass_role(i: u32) -> PassRole {
    if i == 0 {
        return PassRole::Cleanup;
    }
    match (i - 1) % 3 {
        0 => PassRole::Sp,
        1 => PassRole::Mr,
        _ => PassRole::Cleanup,
    }
}

/// Whether the coding pass at absolute index `i` is **terminated** under
/// the §D.6 selective-arithmetic-coding-bypass schedule (Table D.9),
/// combined with the §D.4.2 "termination on each coding pass" flag.
///
/// Per Table D.9 and the §D.6 prose:
///
/// * bit-2 (termination on each coding pass) set → every pass is
///   terminated, "including both raw passes".
/// * otherwise the fourth cleanup pass (`i == 9`) terminates (the
///   `AC, terminate` row that closes the AC region), and from bit-plane
///   5 onward (`i ≥ 10`) every magnitude-refinement raw pass and every
///   cleanup AC pass terminates while the significance-propagation raw
///   pass does not.
pub(crate) fn bypass_pass_terminated(i: u32, termination_on_each_coding_pass: bool) -> bool {
    if termination_on_each_coding_pass {
        return true;
    }
    if i == 9 {
        // Table D.9 fourth cleanup — AC, terminate.
        return true;
    }
    if i >= 10 {
        return matches!(pass_role(i), PassRole::Mr | PassRole::Cleanup);
    }
    false
}

/// Whether the coding pass at absolute index `i` reads its bits from a
/// §D.6 raw (lazy) stream rather than the MQ arithmetic decoder
/// (Table D.9): the SP / MR passes from bit-plane 5 onward (`i ≥ 10`).
/// Cleanup passes are always arithmetic-coded.
pub(crate) fn bypass_pass_is_raw(i: u32) -> bool {
    i >= 10 && matches!(pass_role(i), PassRole::Sp | PassRole::Mr)
}

/// Split a code-block's `passes` coding passes (running from absolute
/// index `start_pass`) into the §B.10.7.2 / Table D.9 bypass codeword
/// segments. Returns one `(span_passes, is_raw)` per segment, in coding
/// order — `span_passes` is how many passes the segment carries and
/// `is_raw` is whether the segment is decoded from a [`crate::t1::RawBitReader`]
/// (its first pass is a raw SP / MR pass) rather than an
/// [`crate::mq::MqDecoder`]. The boundaries are exactly where
/// [`bypass_pass_terminated`] fires, plus the final included pass.
pub(crate) fn bypass_segment_spans(
    start_pass: u32,
    passes: u32,
    termination_on_each_coding_pass: bool,
) -> Vec<(u32, bool)> {
    let mut spans: Vec<(u32, bool)> = Vec::new();
    if passes == 0 {
        return spans;
    }
    let first = start_pass;
    let last = start_pass + passes - 1;
    let mut span_passes = 0u32;
    let mut span_first = first;
    for i in first..=last {
        if span_passes == 0 {
            span_first = i;
        }
        span_passes += 1;
        let terminated = bypass_pass_terminated(i, termination_on_each_coding_pass) || i == last;
        if terminated {
            spans.push((span_passes, bypass_pass_is_raw(span_first)));
            span_passes = 0;
        }
    }
    spans
}

/// T.814 §B.2: whether the HT coding pass with 0-based absolute index
/// `i` ends a codeword segment. `p0_passes` is the number of leading
/// **placeholder passes** (`3·P0`, §B.1) — the boundary set is
/// `T = ⋃ₖ (3P0 + ⌈3k/2⌉) = {3P0, 3P0+2, 3P0+3, 3P0+5, 3P0+6, …}`
/// (`k ≥ 0`), i.e. every HT cleanup pass is its own segment while a
/// SigProp + MagRef pair shares one refinement segment (Figure B.1).
/// The placeholder passes carry no boundary of their own — they fold
/// into the segment ending at the first HT cleanup pass (index `3P0`).
pub(crate) fn ht_pass_ends_segment(i: u32, p0_passes: u32) -> bool {
    i >= p0_passes && (i - p0_passes) % 3 != 1
}

/// Split an HT code-block's `passes` included coding passes (running
/// from absolute 0-based index `start_pass`) into the T.814 §B.2 /
/// §B.3 codeword segments, given the block's `p0_passes` (= `3·P0`)
/// leading placeholder passes: each segment ends at a set-`T` pass or
/// at the final pass included in the packet (§B.3). Returns the pass
/// count of each segment in coding order.
pub(crate) fn ht_segment_spans(start_pass: u32, passes: u32, p0_passes: u32) -> Vec<u32> {
    let mut spans = Vec::new();
    if passes == 0 {
        return spans;
    }
    let last = start_pass + passes - 1;
    let mut span_passes = 0u32;
    for i in start_pass..=last {
        span_passes += 1;
        if ht_pass_ends_segment(i, p0_passes) || i == last {
            spans.push(span_passes);
            span_passes = 0;
        }
    }
    spans
}

/// The absolute pass index at which an HT code-block's **first HT
/// cleanup pass** can sit inside a contribution covering passes
/// `[start_pass, start_pass + passes)`, when every prior pass was a
/// placeholder pass. T.814 §B.1 places the first cleanup pass at
/// index `3·P0` and §B.3 allows at most one HT cleanup pass in the
/// packet that carries it, so the only candidate is the **last**
/// multiple of 3 below the contribution's end (at most two refinement
/// passes may follow it). Returns `None` when no multiple of 3 falls
/// inside the contribution — the contribution can only extend the
/// placeholder run.
pub(crate) fn ht_first_cleanup_candidate(start_pass: u32, passes: u32) -> Option<u32> {
    if passes == 0 {
        return None;
    }
    let cup = ((start_pass + passes - 1) / 3) * 3;
    (cup >= start_pass).then_some(cup)
}

// ---------------------------------------------------------------------------
// Geometry input + per-code-block state — T.800 §B.10.8.
// ---------------------------------------------------------------------------

/// One sub-band's code-block layout within a packet's geometry.
///
/// `width` × `height` is the inclusion / zero-bitplane tag-tree
/// dimension for this sub-band of this precinct, i.e., the number of
/// code-blocks across × down. Empty sub-bands (width or height = 0)
/// are tolerated — they contribute zero entries to the packet header
/// per T.800 §B.6 (empty precincts still produce packets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubBandGeometry {
    /// Code-block grid width (number of code-blocks across).
    pub width: u32,
    /// Code-block grid height (number of code-blocks down).
    pub height: u32,
}

impl SubBandGeometry {
    /// Returns the total code-block count `width * height` (saturating
    /// to u32::MAX on overflow).
    pub fn num_code_blocks(&self) -> u32 {
        self.width.saturating_mul(self.height)
    }
}

/// Per-packet geometry — describes one packet's sub-band → code-block
/// layout. Resolution level 0 has exactly one sub-band (LL); higher
/// resolution levels have three (HL, LH, HH) per T.800 §B.9. Other
/// `sub_bands.len()` values are tolerated as long as the per-sub-band
/// counts agree with the packet's actual byte layout — the reader
/// does not enforce a specific sub-band count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketGeometry {
    /// Sub-bands in the order they appear in the packet (T.800
    /// §B.10.8: "for each sub-band (LL or HL, LH and HH)").
    pub sub_bands: Vec<SubBandGeometry>,
    /// 0-based layer index of this packet within its (precinct,
    /// resolution, component) progression. The decoder uses this as
    /// the inclusion-tag-tree threshold per T.800 §B.10.4.
    pub layer: u16,
}

impl PacketGeometry {
    /// Total number of code-blocks contributed across every sub-band
    /// in this packet. May saturate at `u32::MAX` for hostile inputs.
    pub fn num_code_blocks(&self) -> u32 {
        self.sub_bands
            .iter()
            .fold(0u32, |acc, b| acc.saturating_add(b.num_code_blocks()))
    }
}

/// One code-block's per-packet contribution after the packet header
/// is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeBlockContribution {
    /// Sub-band this code-block lives in, as an index into
    /// [`PacketGeometry::sub_bands`].
    pub sub_band: u32,
    /// 0-based column of the code-block within its sub-band's
    /// `width × height` grid.
    pub x: u32,
    /// 0-based row of the code-block within its sub-band's grid.
    pub y: u32,
    /// `true` iff this packet includes data from this code-block.
    /// If `false`, the segment-length list is empty and no bytes are
    /// drawn from the packet body for this block.
    pub included: bool,
    /// `Mb - P` per T.800 §B.10.5 — the number of zero (missing) most-
    /// significant bit-planes for this code-block. Only filled on the
    /// **first** packet that includes this block; `None` thereafter.
    pub zero_bit_planes: Option<u32>,
    /// Number of coding passes contributed in this packet (§B.10.6).
    /// Zero iff `included` is false.
    pub coding_passes: u32,
    /// Byte lengths of the codeword segments contributed in this
    /// packet (§B.10.7). Sum of these gives the total bytes this
    /// code-block draws from the packet body. Empty iff `included` is
    /// false.
    pub segment_lengths: Vec<u32>,
}

/// Parsed packet header — the structural output of one
/// [`decode_packet_header`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketHeader {
    /// `true` iff the packet's "zero length" bit was 1 (non-empty).
    /// When this is `false`, the [`Self::contributions`] list is
    /// empty.
    pub non_zero_length: bool,
    /// Per-code-block contributions in the order specified by T.800
    /// §B.10.8 — for each sub-band in `geometry.sub_bands`, all
    /// code-blocks in raster order. Length equals
    /// `geometry.num_code_blocks()` iff `non_zero_length` is true.
    pub contributions: Vec<CodeBlockContribution>,
    /// Number of bytes consumed from the input buffer by this packet
    /// header (after byte-aligning the bit reader, per §B.10.1).
    pub bytes_consumed: usize,
    /// Number of code-blocks referenced by `geometry`. Convenience
    /// accessor matching the round-5 task's API shape.
    pub num_code_blocks: u32,
}

impl PacketHeader {
    /// Returns the total byte count drawn from the packet **body**
    /// (post-header) by all included code-blocks combined — i.e.
    /// the sum of every `segment_lengths` entry across every
    /// included contribution.
    pub fn total_body_bytes(&self) -> u64 {
        self.contributions
            .iter()
            .flat_map(|c| c.segment_lengths.iter())
            .fold(0u64, |acc, &len| acc + len as u64)
    }
}

// ---------------------------------------------------------------------------
// Per-code-block state carried across the packets of one precinct.
// ---------------------------------------------------------------------------

/// State per (precinct, sub-band) carried across the packets of all
/// layers — the inclusion tag tree, zero-bitplane tag tree, and per-
/// code-block already-included flag.
#[derive(Debug, Clone)]
pub struct SubBandState {
    /// Inclusion tag tree (T.800 §B.10.4). Threshold queries answer
    /// "has this code-block been included by layer `threshold`?".
    pub inclusion_tree: TagTree,
    /// Zero-bitplane tag tree (T.800 §B.10.5). Full-value read on
    /// first inclusion of each code-block.
    pub zero_bitplane_tree: TagTree,
    /// Per-code-block "already included in a prior packet" flag,
    /// indexed `y * width + x`.
    pub already_included: Vec<bool>,
    /// Per-code-block `Lblock` state (§B.10.7.1).
    pub lblock: Vec<LblockState>,
    /// Per-code-block running count of coding passes already contributed
    /// in **prior** packets (absolute pass cursor, §D.3). Needed by the
    /// §B.10.7.2 / Table D.9 segment split (the terminated-pass set `T`
    /// is keyed off the absolute pass index, which carries across
    /// layers), indexed `y * width + x`.
    pub passes_so_far: Vec<u32>,
    /// T.814 §B.1 placeholder-pass resolution per HT code-block,
    /// indexed `y * width + x`. `None` while the block's first HT
    /// cleanup pass has not yet been seen (every pass contributed so
    /// far was a placeholder pass); `Some(3·P0)` once the first
    /// cleanup pass fixed the placeholder count. Unused outside
    /// [`SegmentSplit::Ht`].
    pub ht_placeholder_passes: Vec<Option<u32>>,
    /// Sub-band grid dimensions.
    pub geometry: SubBandGeometry,
}

impl SubBandState {
    /// Builds fresh per-sub-band state matching `geometry`.
    pub fn new(geometry: SubBandGeometry) -> Self {
        let n = (geometry.width as usize).saturating_mul(geometry.height as usize);
        SubBandState {
            inclusion_tree: TagTree::new(geometry.width, geometry.height),
            zero_bitplane_tree: TagTree::new(geometry.width, geometry.height),
            already_included: vec![false; n],
            lblock: vec![LblockState::default(); n],
            passes_so_far: vec![0u32; n],
            ht_placeholder_passes: vec![None; n],
            geometry,
        }
    }

    fn idx(&self, x: u32, y: u32) -> usize {
        (y as usize) * (self.geometry.width as usize) + (x as usize)
    }
}

/// State across all sub-bands of one precinct — one [`SubBandState`]
/// per sub-band that ever appears in the precinct's packets, in the
/// same order the [`PacketGeometry::sub_bands`] entries appear.
#[derive(Debug, Clone)]
pub struct PrecinctState {
    /// Per-sub-band state. Built lazily by the first
    /// [`decode_packet_header`] call against this precinct so the
    /// sub-band layout can be inferred from that packet's geometry.
    pub sub_bands: Vec<SubBandState>,
}

impl PrecinctState {
    /// Builds empty precinct state — sub-band state is initialised on
    /// the first packet header read for this precinct (so the layout
    /// can be inferred from that packet's [`PacketGeometry`]).
    pub fn new() -> Self {
        PrecinctState {
            sub_bands: Vec::new(),
        }
    }

    /// Ensure the per-sub-band state matches `geometry`. The first
    /// call for this precinct initialises the layout; subsequent
    /// calls must agree (same number of sub-bands, same per-sub-band
    /// dimensions) or [`Error::InvalidPacketHeader`] is returned.
    fn ensure_layout(&mut self, geometry: &PacketGeometry) -> Result<(), Error> {
        if self.sub_bands.is_empty() {
            self.sub_bands = geometry
                .sub_bands
                .iter()
                .map(|g| SubBandState::new(*g))
                .collect();
            return Ok(());
        }
        if self.sub_bands.len() != geometry.sub_bands.len() {
            return Err(Error::InvalidPacketHeader);
        }
        for (st, g) in self.sub_bands.iter().zip(geometry.sub_bands.iter()) {
            if st.geometry != *g {
                return Err(Error::InvalidPacketHeader);
            }
        }
        Ok(())
    }
}

impl Default for PrecinctState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Packet-header decoder — T.800 §B.10.8 (the master order).
// ---------------------------------------------------------------------------

/// Decodes one packet header from the bit-stuffed `bytes` stream
/// against the packet's `geometry` and the running `precinct_state`.
///
/// Returns the parsed [`PacketHeader`] plus updates
/// `precinct_state` for the inclusion + zero-bitplane tag trees and
/// per-code-block Lblock state used by subsequent packets in the same
/// precinct.
///
/// `bytes` should typically be the **remaining tail** of a tile-part
/// body — packet headers can either appear inline (followed by their
/// own data body) or be relocated into a `PPM` / `PPT` segment, in
/// which case `bytes` is the relocated payload. The reader respects
/// the §B.10.1 bit-stuffing rule both ways.
///
/// `sop_eph` selects how the reader treats inter-packet `SOP` and
/// `EPH` markers — see [`SopEphMode`]. The default ([`SopEphMode::None`])
/// is correct for a stream encoded with the COD `Scod` SOP/EPH bits
/// both clear.
///
/// `split` selects the §B.10.7 codeword-segment-length layout per the
/// COD / COC Table A.19 code-block-style bits — see [`SegmentSplit`].
/// [`SegmentSplit::Single`] (the default) reads one length per
/// included contribution; [`SegmentSplit::PerPass`] reads one length
/// per coding pass for the §D.4.2 "termination on each coding pass"
/// style.
pub fn decode_packet_header(
    bytes: &[u8],
    geometry: &PacketGeometry,
    precinct_state: &mut PrecinctState,
    sop_eph: SopEphMode,
    split: SegmentSplit,
) -> Result<PacketHeader, Error> {
    precinct_state.ensure_layout(geometry)?;

    // Optional SOP marker before the packet header (T.800 §A.8.1).
    let mut head = bytes;
    if matches!(sop_eph, SopEphMode::SopOnly | SopEphMode::SopAndEph) {
        head = consume_sop_if_present(head)?;
    }

    let mut reader = PacketBitReader::new(head);
    let zero_length_bit = reader.read_bit()?;
    if zero_length_bit == 0 {
        // §B.10.3: empty packet — no code-blocks contribute. Byte-
        // align and we're done.
        reader.align_to_byte();
        let consumed_in_head = reader.bytes_consumed();
        let mut consumed = (head.as_ptr() as usize) - (bytes.as_ptr() as usize) + consumed_in_head;
        if matches!(sop_eph, SopEphMode::EphOnly | SopEphMode::SopAndEph) {
            consumed += consume_eph_after(bytes, consumed)?;
        }
        return Ok(PacketHeader {
            non_zero_length: false,
            contributions: Vec::new(),
            bytes_consumed: consumed,
            num_code_blocks: geometry.num_code_blocks(),
        });
    }

    let mut contributions = Vec::new();
    for (band_idx, band_geom) in geometry.sub_bands.iter().enumerate() {
        if band_geom.width == 0 || band_geom.height == 0 {
            continue;
        }
        for y in 0..band_geom.height {
            for x in 0..band_geom.width {
                let contribution = decode_one_code_block(
                    &mut precinct_state.sub_bands[band_idx],
                    band_idx as u32,
                    x,
                    y,
                    geometry.layer,
                    &mut reader,
                    split,
                )?;
                contributions.push(contribution);
            }
        }
    }

    reader.align_to_byte();
    let consumed_in_head = reader.bytes_consumed();
    let mut consumed = (head.as_ptr() as usize) - (bytes.as_ptr() as usize) + consumed_in_head;
    if matches!(sop_eph, SopEphMode::EphOnly | SopEphMode::SopAndEph) {
        consumed += consume_eph_after(bytes, consumed)?;
    }

    Ok(PacketHeader {
        non_zero_length: true,
        num_code_blocks: contributions.len() as u32,
        contributions,
        bytes_consumed: consumed,
    })
}

/// Read one code-block's contribution out of the bit stream.
fn decode_one_code_block(
    sub_band: &mut SubBandState,
    band_idx: u32,
    x: u32,
    y: u32,
    layer: u16,
    reader: &mut PacketBitReader<'_>,
    split: SegmentSplit,
) -> Result<CodeBlockContribution, Error> {
    let idx = sub_band.idx(x, y);
    let already_in = sub_band.already_included[idx];

    let (included, zero_bit_planes) = if already_in {
        // §B.10.4: one bit — 1 = included this layer, 0 = not.
        let bit = reader.read_bit()?;
        (bit == 1, None)
    } else {
        // §B.10.4: inclusion-tag-tree query at threshold = layer + 1.
        // A code-block whose tree value is <= layer is included.
        let included_now =
            sub_band
                .inclusion_tree
                .decode_below_threshold(x, y, (layer as u32) + 1, reader)?;
        if included_now {
            // §B.10.5: read the zero-bitplane value (full decode of
            // the leaf in the zero-bitplane tag tree).
            let p = sub_band.zero_bitplane_tree.decode_value(x, y, reader)?;
            (true, Some(p))
        } else {
            (false, None)
        }
    };

    if !included {
        return Ok(CodeBlockContribution {
            sub_band: band_idx,
            x,
            y,
            included: false,
            zero_bit_planes,
            coding_passes: 0,
            segment_lengths: Vec::new(),
        });
    }

    // Mark as included for subsequent layers.
    sub_band.already_included[idx] = true;

    // §B.10.6 — number of coding passes.
    let passes = decode_coding_passes(reader)?;

    // Absolute pass cursor for this code-block at the **start** of this
    // packet (the count of passes contributed by prior packets). The
    // §B.10.7.2 / Table D.9 bypass split keys off the absolute pass
    // index, so it carries across layers.
    let start_pass = sub_band.passes_so_far[idx];
    sub_band.passes_so_far[idx] = start_pass
        .checked_add(passes)
        .ok_or(Error::InvalidPacketHeader)?;

    // §B.10.7 — codeword-segment lengths. `split` decides how the
    // contribution's `passes` map onto §C.3 codeword segments:
    //
    // * `Single` (§B.10.7.1) — all passes form one codeword segment;
    //   read one length sized for the whole pass count.
    // * `PerPass` (§B.10.7.2 with the COD / COC Table A.19 bit-2
    //   "termination on each coding pass" flag) — every included pass
    //   is terminated, so `T` is the full pass-index set and `K =
    //   passes`. Read `passes` lengths, each covering exactly one pass
    //   (Equation B-19 widening `floor(log2 1) = 0`). The §B.10.7.1
    //   Lblock signalling prefix is read once per length (the worked
    //   §B.10.7.2 example increments Lblock only on the first length,
    //   but the spec permits a zero increment on the rest, which
    //   `read_lblock_increment` handles transparently).
    let lblock = &mut sub_band.lblock[idx];
    let segment_lengths = match split {
        SegmentSplit::Single => vec![decode_segment_length(lblock, passes, reader)?],
        SegmentSplit::PerPass => {
            // §B.10.7.2: the increase-Lblock prefix is signalled **once**
            // before the first length; the remaining K − 1 lengths reuse
            // the same Lblock. Every terminated pass carries exactly one
            // coding pass, so each width's Equation B-19 widening is
            // `floor(log2 1) = 0` (`passes_in_segment = 1`).
            read_lblock_increment(lblock, reader)?;
            let mut lens = Vec::with_capacity(passes as usize);
            for _ in 0..passes {
                lens.push(read_segment_length_value(lblock.lblock, 1, reader)?);
            }
            lens
        }
        SegmentSplit::Bypass {
            termination_on_each_coding_pass,
        } => {
            // §B.10.7.2 / Table D.9: `T` is the set of **terminated**
            // passes among those included in this packet, plus the final
            // included pass if it is not already terminated. The passes
            // run from absolute index `start_pass` to
            // `start_pass + passes − 1`. Each signalled length covers the
            // span from the previous boundary up to and including the
            // next terminated pass; Equation B-19 widens by
            // `floor(log2(passes_in_that_span))`. The increase-Lblock
            // prefix is signalled **once** before the first length.
            let spans = bypass_segment_spans(start_pass, passes, termination_on_each_coding_pass);
            read_lblock_increment(lblock, reader)?;
            let mut lens = Vec::with_capacity(spans.len());
            for (sp, _is_raw) in spans {
                lens.push(read_segment_length_value(lblock.lblock, sp, reader)?);
            }
            lens
        }
        SegmentSplit::Ht => {
            // T.814 §B.2 / §B.3: segments end at the set-T passes (or
            // the final included pass); one length per segment, widened
            // by ⌊log2(passes in segment)⌋ (Equation B-19), with the
            // increase-Lblock prefix signalled once per contribution.
            //
            // The set T is anchored at the block's 3·P0 placeholder
            // passes (§B.1). P0 is not signalled anywhere — it is
            // pinned the moment the first HT cleanup pass appears: the
            // §B.3 one-cleanup-per-first-packet rule leaves exactly one
            // candidate index (the last multiple of 3 inside the
            // contribution), and the first HT cleanup segment's length
            // is required to exceed 1 (§B.3) while a placeholder pass
            // carries no bytes at all, so a first length field of 0
            // *is* a placeholder run and a first length field ≥ 2 *is*
            // the cleanup segment. The two readings only differ in the
            // Equation B-19 widening of the first field — the
            // cleanup-candidate reading is never wider, so the prefix
            // bits are read at the narrow width and the residual
            // widening bits (which a placeholder run's zero field pads
            // with zeros) are consumed after the fact.
            match sub_band.ht_placeholder_passes[idx] {
                Some(p0) => {
                    let spans = ht_segment_spans(start_pass, passes, p0);
                    read_lblock_increment(lblock, reader)?;
                    let mut lens = Vec::with_capacity(spans.len());
                    for sp in spans {
                        lens.push(read_segment_length_value(lblock.lblock, sp, reader)?);
                    }
                    lens
                }
                None => {
                    read_lblock_increment(lblock, reader)?;
                    match ht_first_cleanup_candidate(start_pass, passes) {
                        None => {
                            // No multiple of 3 inside the contribution:
                            // it can only extend the placeholder run —
                            // one zero-length segment (§B.1).
                            let len = read_segment_length_value(lblock.lblock, passes, reader)?;
                            if len != 0 {
                                return Err(Error::InvalidPacketHeader);
                            }
                            vec![0]
                        }
                        Some(cup) => {
                            // Trial-read the first field at the
                            // cleanup-candidate width.
                            let span1 = cup - start_pass + 1;
                            let narrow = segment_length_width(lblock.lblock, span1)?;
                            let v = reader.read_bits(narrow)?;
                            if v >= 2 {
                                // First HT cleanup segment (covers the
                                // placeholder run, if any, plus the
                                // cleanup pass at index `cup` = 3·P0).
                                sub_band.ht_placeholder_passes[idx] = Some(cup);
                                let rem = start_pass + passes - 1 - cup;
                                let mut lens = vec![v];
                                if rem > 0 {
                                    lens.push(read_segment_length_value(
                                        lblock.lblock,
                                        rem,
                                        reader,
                                    )?);
                                }
                                lens
                            } else if v == 0 {
                                // Placeholder run: the single field was
                                // written at the span-`passes` width;
                                // consume the residual low bits (all
                                // zero for a zero length).
                                let wide = segment_length_width(lblock.lblock, passes)?;
                                if wide > narrow && reader.read_bits(wide - narrow)? != 0 {
                                    return Err(Error::InvalidPacketHeader);
                                }
                                vec![0]
                            } else {
                                // A first HT cleanup segment of length 1
                                // violates §B.3 under either reading.
                                return Err(Error::InvalidPacketHeader);
                            }
                        }
                    }
                }
            }
        }
    };

    Ok(CodeBlockContribution {
        sub_band: band_idx,
        x,
        y,
        included: true,
        zero_bit_planes,
        coding_passes: passes,
        segment_lengths,
    })
}

/// Modes for handling SOP / EPH markers around a packet header.
///
/// T.800 §A.6.1 says the `Scod` field's `0x02` bit enables SOP
/// markers and the `0x04` bit enables EPH markers. The bit reader is
/// driven by the caller; this enum lets the caller declare how the
/// stream is framed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SopEphMode {
    /// Neither SOP nor EPH markers present (default).
    #[default]
    None,
    /// SOP markers may precede each packet, EPH markers absent.
    SopOnly,
    /// EPH markers may follow each packet header, SOP absent.
    EphOnly,
    /// Both SOP (preceding) and EPH (following) markers present.
    SopAndEph,
}

/// How a code-block contribution to one packet splits into codeword
/// segments (T.800 §B.10.7).
///
/// The COD / COC Table A.19 code-block-style bits decide whether the
/// passes contributed in a packet form a single §C.3 codeword segment
/// or several. The packet-header reader needs this to know how many
/// §B.10.7.1 lengths to read for each included contribution and how to
/// size each one (Equation B-19 widens by `floor(log2 passes)` of the
/// passes in *that* segment).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SegmentSplit {
    /// §B.10.7.1 — all of a contribution's passes form one codeword
    /// segment. One length is signalled, sized for the whole pass
    /// count. This is the default (no termination / no AC bypass).
    #[default]
    Single,
    /// §B.10.7.2 with the COD / COC Table A.19 bit-2
    /// "termination on each coding pass" flag set (§D.4.2): **every**
    /// included pass is terminated, so `T` is the full set of pass
    /// indices and `K` equals the contribution's pass count. Each of
    /// the `K` lengths covers exactly one pass (Equation B-19 widening
    /// is `floor(log2 1) = 0`).
    PerPass,
    /// T.800 §D.6 selective arithmetic-coding bypass (Table A.19 bit 0).
    /// The code-block contribution carves into AC and raw (lazy)
    /// codeword segments per Table D.9: the SP / MR passes from
    /// bit-plane 5 onward read raw bits, the cleanup passes stay AC, and
    /// the §B.10.7.2 terminated-pass set `T` is the union of every
    /// terminated pass (the fourth cleanup; from bit-plane 5 each MR raw
    /// and cleanup AC pass) plus the final included pass. The number of
    /// §B.10.7 lengths equals `|T|` over the passes included in this
    /// packet, each sized for the passes it spans (Equation B-19).
    ///
    /// `termination_on_each_coding_pass` carries the COD / COC Table
    /// A.19 bit-2 flag, which — when also set — terminates **every**
    /// pass (including both raw passes), per the §D.6 prose.
    Bypass {
        /// Whether the §D.4.2 bit-2 "termination on each coding pass"
        /// flag is also set (composes with bypass).
        termination_on_each_coding_pass: bool,
    },
    /// T.814 §B.2 / §B.3 — the tile-component's code-blocks are HT
    /// code-blocks (SPcod bit 6). A contribution splits at the set-`T`
    /// boundaries `{3P0, 3P0+2, 3P0+3, 3P0+5, 3P0+6, …}` over the
    /// absolute pass indices (anchored at the block's `3·P0` §B.1
    /// placeholder passes): each HT cleanup pass is one codeword
    /// segment and each SigProp (+ MagRef) pair is one refinement
    /// segment, so the number of §B.10.7 lengths follows
    /// [`ht_segment_spans`] (each widened by
    /// `⌊log2(passes in that segment)⌋` per Equation B-19). `P0` is
    /// pinned when the first HT cleanup pass appears (its segment
    /// length exceeds 1 while a placeholder run's is 0, §B.3).
    Ht,
}

/// Marker code — SOP (Start of packet, T.800 §A.8.1, `0xFF91`).
pub(crate) const MARKER_SOP: u16 = 0xFF91;
/// Marker code — EPH (End of packet header, T.800 §A.8.2, `0xFF92`).
pub(crate) const MARKER_EPH: u16 = 0xFF92;

/// If the next 2 bytes are an SOP marker, consume the whole SOP
/// segment (marker + Lsop=4 + Nsop=2 bytes = 6 bytes total per T.800
/// Table A.41bis). Otherwise leave the slice unchanged.
fn consume_sop_if_present(bytes: &[u8]) -> Result<&[u8], Error> {
    if bytes.len() < 2 {
        return Ok(bytes);
    }
    let marker = u16::from_be_bytes([bytes[0], bytes[1]]);
    if marker != MARKER_SOP {
        return Ok(bytes);
    }
    if bytes.len() < 6 {
        return Err(Error::InvalidPacketHeader);
    }
    let lsop = u16::from_be_bytes([bytes[2], bytes[3]]);
    if lsop != 4 {
        return Err(Error::InvalidPacketHeader);
    }
    Ok(&bytes[6..])
}

/// Peek the `Nsop` packet sequence number of a leading SOP marker
/// **without consuming** it. Returns `None` when no SOP is present (the
/// SOP is optional per-packet even when allowed, T.800 §A.8.1) or the
/// slice is too short to hold a marker code.
///
/// A malformed SOP (wrong `Lsop`, or a marker code with fewer than the
/// fixed six bytes following) is surfaced as [`Error::InvalidPacketHeader`]
/// — the subsequent [`consume_sop_if_present`] would reject it anyway, but
/// validating here keeps the §A.8.1 `Nsop` check self-contained.
fn peek_sop_nsop(bytes: &[u8]) -> Result<Option<u16>, Error> {
    if bytes.len() < 2 {
        return Ok(None);
    }
    if u16::from_be_bytes([bytes[0], bytes[1]]) != MARKER_SOP {
        return Ok(None);
    }
    if bytes.len() < 6 {
        return Err(Error::InvalidPacketHeader);
    }
    if u16::from_be_bytes([bytes[2], bytes[3]]) != 4 {
        return Err(Error::InvalidPacketHeader);
    }
    Ok(Some(u16::from_be_bytes([bytes[4], bytes[5]])))
}

/// After the packet header's bit reader byte-aligns, consume an
/// optional EPH marker (2 bytes) at the slice tail. Returns the
/// number of bytes consumed (0 or 2).
fn consume_eph_after(bytes: &[u8], offset: usize) -> Result<usize, Error> {
    if offset + 2 > bytes.len() {
        return Ok(0);
    }
    let marker = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]);
    if marker == MARKER_EPH {
        Ok(2)
    } else {
        Ok(0)
    }
}

// ---------------------------------------------------------------------------
// Multi-packet walker.
// ---------------------------------------------------------------------------

/// Walks a series of packet headers across one tile-part's body
/// according to a caller-supplied list of `(precinct-state id,
/// [`PacketGeometry`], [`SegmentSplit`])` entries (one per packet, in
/// codestream order — i.e. the progression order from T.800 §B.12).
/// The split is per packet because it follows the packet's
/// component's resolved Table A.19 style byte (§A.6.2 — components
/// may diverge, e.g. the T.814 HTDECLARED HT / Annex D mix).
///
/// The walker maintains a `(precinct_index → PrecinctState)` map; each
/// packet's `precinct_index` selects which precinct's tag-tree state
/// applies. Within one (component, resolution, precinct) progression
/// the inclusion tag tree's state is preserved across layers per
/// T.800 §B.10.2, so the walker can decode many layers' packets in
/// sequence as long as the caller groups them under the same
/// `precinct_index`.
///
/// `sop_eph` declares the framing per [`SopEphMode`].
///
/// Returns one [`PacketHeader`] per geometry entry; the sum of every
/// entry's `bytes_consumed` plus every code-block contribution's
/// segment-length bytes equals the tile-part body length (modulo any
/// trailing padding the encoder placed).
pub fn walk_packet_headers(
    body: &[u8],
    packets: &[(usize, PacketGeometry, SegmentSplit)],
    sop_eph: SopEphMode,
) -> Result<Vec<PacketHeader>, Error> {
    // We don't know up-front how many distinct precinct_index values
    // appear; collect into a sparse map.
    let mut precincts: std::collections::HashMap<usize, PrecinctState> =
        std::collections::HashMap::new();
    let mut out = Vec::with_capacity(packets.len());
    let mut pos = 0usize;
    // §A.8.1 Nsop: the first packet in a coded tile is packet 0, and the
    // count increments by one for *every* successive packet whether or
    // not an SOP marker is actually emitted, rolling over at 65 536. When
    // SOP framing is enabled and a packet carries an SOP, its Nsop must
    // equal this running ordinal — a mismatch flags a desynchronised /
    // lost packet, so it is rejected rather than silently mis-decoded.
    let sop_allowed = matches!(sop_eph, SopEphMode::SopOnly | SopEphMode::SopAndEph);
    for (packet_ordinal, (precinct_index, geometry, split)) in packets.iter().enumerate() {
        if pos > body.len() {
            return Err(Error::PacketHeaderOverrun);
        }
        if sop_allowed {
            if let Some(nsop) = peek_sop_nsop(&body[pos..])? {
                // The §A.8.1 counter wraps at 2^16.
                let expected = (packet_ordinal % 65_536) as u16;
                if nsop != expected {
                    return Err(Error::InvalidPacketHeader);
                }
            }
        }
        let state = precincts.entry(*precinct_index).or_default();
        let header = decode_packet_header(&body[pos..], geometry, state, sop_eph, *split)?;
        pos = pos
            .checked_add(header.bytes_consumed)
            .ok_or(Error::PacketHeaderOverrun)?;
        // Skip the body bytes drawn by this packet.
        let body_bytes = header.total_body_bytes();
        let body_bytes = usize::try_from(body_bytes).map_err(|_| Error::PacketHeaderOverrun)?;
        pos = pos
            .checked_add(body_bytes)
            .ok_or(Error::PacketHeaderOverrun)?;
        if pos > body.len() {
            return Err(Error::PacketHeaderOverrun);
        }
        out.push(header);
    }
    Ok(out)
}

/// One packet's parsed header plus the byte offset, **within the
/// tile-part body**, at which the packet's code-block data begins.
///
/// Used by the relocated-header ([`walk_packet_headers_separate`])
/// path: with `PPM` / `PPT` in play the header stream and the data
/// stream live in distinct buffers, so the caller cannot recover the
/// data start from the in-stream `PacketHeader::bytes_consumed`. This
/// pairs each header with its concrete data offset instead.
#[derive(Debug, Clone)]
pub struct RelocatedPacket {
    /// The parsed packet header (decoded out of the relocated
    /// `PPM` / `PPT` header buffer).
    pub header: PacketHeader,
    /// Offset into the tile-part **body** at which this packet's
    /// code-block segment data begins (after any in-body `SOP`).
    pub body_offset: usize,
}

/// Walks a series of packet headers when they have been **relocated**
/// out of the bit-stream into `PPM` / `PPT` marker segments
/// (T.800 §A.7.4 / §A.7.5).
///
/// Unlike [`walk_packet_headers`], the headers and the packet data
/// occupy two separate buffers:
///
/// * `header_bytes` — the concatenated `Ippm` / `Ippt` payload (all
///   segments in increasing `Zppm` / `Zppt` order). Each packet header
///   is decoded from here; the `§B.10.1` bit-stuffing rule and (when
///   `EPH` is signalled) the trailing `EPH` marker live in this buffer
///   per §A.8.2.
/// * `body` — the tile-part compressed-data portion, which now holds
///   *only* packet data (no in-stream headers). Per §A.8.1 an optional
///   `SOP` marker segment may still precede each packet's data here.
///
/// Returns one [`RelocatedPacket`] per geometry entry, each carrying
/// the parsed header and the body offset at which that packet's
/// code-block data begins. The caller slices code-block segments out
/// of `body` starting at `body_offset`.
pub fn walk_packet_headers_separate(
    header_bytes: &[u8],
    body: &[u8],
    packets: &[(usize, PacketGeometry, SegmentSplit)],
    sop_eph: SopEphMode,
) -> Result<Vec<RelocatedPacket>, Error> {
    // §A.8.1 / §A.8.2: when headers are relocated, SOP (if allowed)
    // sits in the body before each packet's data, and EPH (if
    // required) sits in the header buffer after each header. Split the
    // mode so the header decoder sees only EPH and the body walk sees
    // only SOP.
    let header_mode = match sop_eph {
        SopEphMode::EphOnly | SopEphMode::SopAndEph => SopEphMode::EphOnly,
        _ => SopEphMode::None,
    };
    let sop_in_body = matches!(sop_eph, SopEphMode::SopOnly | SopEphMode::SopAndEph);

    let mut precincts: std::collections::HashMap<usize, PrecinctState> =
        std::collections::HashMap::new();
    let mut out = Vec::with_capacity(packets.len());
    let mut hpos = 0usize;
    let mut bpos = 0usize;
    for (packet_ordinal, (precinct_index, geometry, split)) in packets.iter().enumerate() {
        if hpos > header_bytes.len() || bpos > body.len() {
            return Err(Error::PacketHeaderOverrun);
        }
        // §A.8.1: an SOP in the body must carry the running ordinal.
        if sop_in_body {
            if let Some(nsop) = peek_sop_nsop(&body[bpos..])? {
                let expected = (packet_ordinal % 65_536) as u16;
                if nsop != expected {
                    return Err(Error::InvalidPacketHeader);
                }
                // Consume the six-byte SOP marker segment from the body.
                bpos = bpos.checked_add(6).ok_or(Error::PacketHeaderOverrun)?;
            }
        }
        let state = precincts.entry(*precinct_index).or_default();
        let header =
            decode_packet_header(&header_bytes[hpos..], geometry, state, header_mode, *split)?;
        hpos = hpos
            .checked_add(header.bytes_consumed)
            .ok_or(Error::PacketHeaderOverrun)?;
        if hpos > header_bytes.len() {
            return Err(Error::PacketHeaderOverrun);
        }
        let body_offset = bpos;
        let body_bytes = header.total_body_bytes();
        let body_bytes = usize::try_from(body_bytes).map_err(|_| Error::PacketHeaderOverrun)?;
        bpos = bpos
            .checked_add(body_bytes)
            .ok_or(Error::PacketHeaderOverrun)?;
        if bpos > body.len() {
            return Err(Error::PacketHeaderOverrun);
        }
        out.push(RelocatedPacket {
            header,
            body_offset,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Packet-header ENCODER — the §B.10 write side.
// ---------------------------------------------------------------------------
//
// The encode-side mirrors of the reader above, used by the encoder path:
// a bit-stuffing writer (§B.10.1), a tag-tree encoder (§B.10.2) whose bit
// output the `TagTree` decoder reproduces exactly, the Table B.4
// coding-passes codeword writer (§B.10.6), the Lblock segment-length
// writer (§B.10.7.1), and `encode_packet_header` composing them in the
// §B.10.8 master order (single-codeword-segment / `SegmentSplit::Single`
// layout, no SOP / EPH framing — the caller wraps the output if needed).

/// Bit-stuffed packet-header **writer** — the §B.10.1 inverse of
/// [`PacketBitReader`]. Bits are packed MSB-first; after any produced
/// byte equals `0xFF`, a zero stuff bit is inserted at the MSB of the
/// following byte. `finish` pads the final partial byte with zeros
/// (honouring the stuff rule) and returns the buffer.
#[derive(Debug, Clone, Default)]
pub struct PacketBitWriter {
    out: Vec<u8>,
    /// Bits accumulated into the current byte (MSB-first).
    cur: u8,
    /// Number of bits currently held in `cur` (0..8, where a fresh byte
    /// following a `0xFF` starts at 1 — the stuff bit — not 0).
    bits_used: u8,
}

impl PacketBitWriter {
    /// Fresh writer with an empty output buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one bit (`0` or `1`).
    pub fn write_bit(&mut self, bit: u8) {
        if self.bits_used == 0 && self.out.last() == Some(&0xFF) {
            // §B.10.1 stuff: the byte after a 0xFF starts with a zero bit.
            self.cur = 0;
            self.bits_used = 1;
        }
        self.cur = (self.cur << 1) | (bit & 1);
        self.bits_used += 1;
        if self.bits_used == 8 {
            self.out.push(self.cur);
            self.cur = 0;
            self.bits_used = 0;
        }
    }

    /// Append the low `n` bits of `v`, most significant first.
    pub fn write_bits(&mut self, v: u32, n: u8) {
        for i in (0..n).rev() {
            self.write_bit(((v >> i) & 1) as u8);
        }
    }

    /// Pad the final partial byte with zero bits to the byte boundary
    /// (§B.10.1 — a packet header is a whole number of bytes) and return
    /// the buffer. A trailing `0xFF` cannot arise from zero padding, so
    /// the §B.10.1 "packet header shall not end with 0xFF" rule holds.
    pub fn finish(mut self) -> Vec<u8> {
        if self.bits_used > 0 {
            self.cur <<= 8 - self.bits_used;
            self.out.push(self.cur);
        }
        self.out
    }
}

/// Tag-tree **encoder** (T.800 §B.10.2) — the write-side mirror of
/// [`TagTree`].
///
/// Construction takes the full leaf-value grid up front; each internal
/// node's value is the minimum of its (up to four) children per §B.10.2.
/// `encode_below_threshold` / `encode_value` then emit exactly the bits
/// the decoder's `decode_below_threshold` / `decode_value` will consume,
/// carrying the same per-node committed state across calls so repeated /
/// interleaved queries stay in lock-step with the reader.
#[derive(Debug, Clone)]
pub struct TagTreeEncoder {
    width: u32,
    height: u32,
    /// Actual node values, root level first (same layout as
    /// [`TagTree::levels`]).
    values: Vec<Vec<u32>>,
    /// Per-node `(reached, committed)` write state mirroring the
    /// decoder's `(value, fully_decoded)`.
    state: Vec<Vec<(u32, bool)>>,
}

impl TagTreeEncoder {
    /// Build the encoder over a `width × height` leaf grid whose values
    /// are `leaves[y * width + x]`.
    ///
    /// A zero-dimension grid yields an empty tree (mirroring
    /// [`TagTree::new`]); its encode methods must not be called — the
    /// §B.10.8 walk skips empty sub-bands, matching the reader.
    ///
    /// Panics if `leaves.len() != width * height`.
    pub fn new(width: u32, height: u32, leaves: &[u32]) -> Self {
        assert_eq!(leaves.len(), (width as usize) * (height as usize));
        if width == 0 || height == 0 {
            return TagTreeEncoder {
                width,
                height,
                values: Vec::new(),
                state: Vec::new(),
            };
        }
        // Build the per-level dimensions leaf-first, then compute each
        // coarser level as the min of its children.
        let mut dims: Vec<(u32, u32)> = Vec::new();
        let mut w = width;
        let mut h = height;
        dims.push((w, h));
        while w > 1 || h > 1 {
            w = w.div_ceil(2);
            h = h.div_ceil(2);
            dims.push((w, h));
        }
        let mut values_leaf_first: Vec<Vec<u32>> = Vec::with_capacity(dims.len());
        values_leaf_first.push(leaves.to_vec());
        for lev in 1..dims.len() {
            let (cw, ch) = dims[lev];
            let (fw, fh) = dims[lev - 1];
            let finer = &values_leaf_first[lev - 1];
            let mut coarse = vec![u32::MAX; (cw as usize) * (ch as usize)];
            for fy in 0..fh {
                for fx in 0..fw {
                    let ci = ((fy / 2) as usize) * (cw as usize) + (fx / 2) as usize;
                    let v = finer[(fy as usize) * (fw as usize) + fx as usize];
                    if v < coarse[ci] {
                        coarse[ci] = v;
                    }
                }
            }
            values_leaf_first.push(coarse);
        }
        values_leaf_first.reverse(); // root-first, matching TagTree.
        let state = values_leaf_first
            .iter()
            .map(|lvl| vec![(0u32, false); lvl.len()])
            .collect();
        TagTreeEncoder {
            width,
            height,
            values: values_leaf_first,
            state,
        }
    }

    fn level_width(&self, level: usize) -> u32 {
        let depth = self.values.len();
        let shift = (depth - 1 - level) as u32;
        let mut w = self.width;
        for _ in 0..shift {
            w = w.div_ceil(2);
        }
        w
    }

    /// Emit the bits that let the decoder answer "is leaf `(x, y)` below
    /// `threshold`?" — the write-side mirror of
    /// [`TagTree::decode_below_threshold`]. Walks root → leaf; at each
    /// node emits `0` while the running lower bound is below both the
    /// node's actual value and the threshold, and `1` when the bound
    /// reaches the actual value (committing it). Stops early once the
    /// bound reaches the threshold uncommitted (the decoder then knows
    /// `value ≥ threshold`).
    pub fn encode_below_threshold(
        &mut self,
        x: u32,
        y: u32,
        threshold: u32,
        writer: &mut PacketBitWriter,
    ) {
        assert!(x < self.width && y < self.height);
        let depth = self.values.len();
        let mut value_above: u32 = 0;
        for level in 0..depth {
            let shift = (depth - 1 - level) as u32;
            let lw = self.level_width(level);
            let node_idx = ((y >> shift) as usize) * (lw as usize) + (x >> shift) as usize;
            let actual = self.values[level][node_idx];
            let (mut v, mut committed) = self.state[level][node_idx];
            if v < value_above {
                v = value_above;
            }
            while !committed && v < threshold {
                if v < actual {
                    writer.write_bit(0);
                    v += 1;
                } else {
                    writer.write_bit(1);
                    committed = true;
                }
            }
            self.state[level][node_idx] = (v, committed);
            if !committed {
                return; // bound reached threshold; decoder infers ≥.
            }
            value_above = v;
        }
    }

    /// Emit the bits that commit leaf `(x, y)`'s exact value — the
    /// write-side mirror of [`TagTree::decode_value`].
    pub fn encode_value(&mut self, x: u32, y: u32, writer: &mut PacketBitWriter) {
        assert!(x < self.width && y < self.height);
        let depth = self.values.len();
        let mut value_above: u32 = 0;
        for level in 0..depth {
            let shift = (depth - 1 - level) as u32;
            let lw = self.level_width(level);
            let node_idx = ((y >> shift) as usize) * (lw as usize) + (x >> shift) as usize;
            let actual = self.values[level][node_idx];
            let (mut v, mut committed) = self.state[level][node_idx];
            if v < value_above {
                v = value_above;
            }
            while !committed {
                if v < actual {
                    writer.write_bit(0);
                    v += 1;
                } else {
                    writer.write_bit(1);
                    committed = true;
                }
            }
            self.state[level][node_idx] = (v, committed);
            value_above = v;
        }
    }
}

/// Encode the number of coding passes per T.800 §B.10.6 / Table B.4 —
/// the inverse of [`decode_coding_passes`]. `passes` must be in
/// `1..=164`; out-of-range values are a caller bug and panic.
pub fn encode_coding_passes(passes: u32, writer: &mut PacketBitWriter) {
    match passes {
        1 => writer.write_bit(0),
        2 => writer.write_bits(0b10, 2),
        3 => writer.write_bits(0b1100, 4),
        4 => writer.write_bits(0b1101, 4),
        5 => writer.write_bits(0b1110, 4),
        6..=36 => {
            writer.write_bits(0b1111, 4);
            writer.write_bits(passes - 6, 5);
        }
        37..=164 => {
            writer.write_bits(0b1111, 4);
            writer.write_bits(31, 5);
            writer.write_bits(passes - 37, 7);
        }
        _ => panic!("coding passes {passes} outside Table B.4 range 1..=164"),
    }
}

/// Encode one §B.10.7.1 codeword-segment length, choosing the minimal
/// `Lblock` increase so `length` fits in
/// `Lblock + floor(log2 passes_in_segment)` bits — the inverse of
/// [`decode_segment_length`]. Emits `k` one-bits + a terminating zero
/// (the increase-Lblock prefix) followed by the big-endian length field,
/// and updates `state` exactly as the reader will.
pub fn encode_segment_length(
    state: &mut LblockState,
    passes_in_segment: u32,
    length: u32,
    writer: &mut PacketBitWriter,
) {
    let extra = if passes_in_segment <= 1 {
        0
    } else {
        passes_in_segment.ilog2()
    };
    // Minimal bit-width that represents `length` (at least 1).
    let needed = if length == 0 {
        1
    } else {
        32 - length.leading_zeros()
    };
    let k = needed.saturating_sub(state.lblock + extra);
    for _ in 0..k {
        writer.write_bit(1);
    }
    writer.write_bit(0);
    state.lblock += k;
    writer.write_bits(length, (state.lblock + extra) as u8);
}

/// Encode the §B.10.7 codeword-segment length sequence of one
/// code-block contribution — the write-side counterpart of the
/// [`SegmentSplit`]-driven reads in [`decode_packet_header`].
///
/// `segments` lists `(passes_in_segment, byte_length)` per codeword
/// segment in coding order: one entry for the [`SegmentSplit::Single`]
/// layout, one per pass for [`SegmentSplit::PerPass`], and one per
/// Table D.9 span for [`SegmentSplit::Bypass`] (the caller derives the
/// spans with the same rule the reader uses). Per §B.10.7.2 the
/// increase-`Lblock` prefix is signalled **once** before the first
/// length, with `k` chosen minimal so *every* length fits its
/// `Lblock + ⌊log2 passes⌋` field; the reader's per-length prefix reads
/// then see "no further increase" zero bits, which
/// [`decode_segment_length`] / the multi-segment reads consume
/// transparently... except that the reader reads the prefix only once
/// too, so the writer emits exactly one prefix. `state` updates in
/// lock-step with the reader.
pub fn encode_segment_lengths(
    state: &mut LblockState,
    segments: &[(u32, u32)],
    writer: &mut PacketBitWriter,
) {
    let width_extra = |p: u32| -> u32 {
        if p <= 1 {
            0
        } else {
            p.ilog2()
        }
    };
    // Minimal k so every length fits (and no field exceeds 32 bits).
    let mut k = 0u32;
    for &(p, len) in segments {
        let needed = if len == 0 {
            1
        } else {
            32 - len.leading_zeros()
        };
        let have = state.lblock + width_extra(p);
        if needed > have + k {
            k = needed - have;
        }
    }
    for _ in 0..k {
        writer.write_bit(1);
    }
    writer.write_bit(0);
    state.lblock += k;
    for &(p, len) in segments {
        writer.write_bits(len, (state.lblock + width_extra(p)) as u8);
    }
}

/// One code-block's contribution to a packet, on the **encode** side —
/// the write-side counterpart of [`CodeBlockContribution`]. Blocks are
/// listed in the §B.10.8 order (per sub-band, raster within the
/// sub-band); a block that contributes nothing this layer sets
/// `included: false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeBlockPlan {
    /// Whether this packet includes data from this code-block.
    pub included: bool,
    /// The §B.10.5 number of zero (missing) most-significant bit-planes
    /// `P` for this code-block. Read from the plan only on the block's
    /// **first** inclusion (it seeds the zero-bitplane tag tree for the
    /// whole precinct up front, so it must be present — even on plans
    /// whose first inclusion happens in a later layer).
    pub zero_bit_planes: u32,
    /// Number of coding passes contributed in this packet (§B.10.6).
    /// Must be ≥ 1 when `included`.
    pub coding_passes: u32,
    /// The contribution's codeword segments, `(passes_in_segment,
    /// byte_length)` in coding order (§B.10.7): a single entry spanning
    /// all `coding_passes` under [`SegmentSplit::Single`], one entry
    /// per pass under [`SegmentSplit::PerPass`], one per Table D.9 span
    /// under [`SegmentSplit::Bypass`]. The pass counts must sum to
    /// `coding_passes`.
    pub segments: Vec<(u32, u32)>,
}

/// Per-precinct **encoder** state carried across the packets (layers)
/// of one precinct — the write-side counterpart of [`PrecinctState`].
///
/// Unlike the decoder (which grows its tag trees lazily), the encoder
/// must know every code-block's inclusion layer and zero-bitplane count
/// up front — the §B.10.2 tag-tree minima depend on the whole grid.
/// Build it once per precinct via [`PrecinctEncoderState::new`], then
/// call [`encode_packet_header`] once per layer in order.
#[derive(Debug, Clone)]
pub struct PrecinctEncoderState {
    /// Per-sub-band: inclusion tag-tree encoder (leaf = first inclusion
    /// layer), zero-bitplane tag-tree encoder (leaf = `P`), per-block
    /// already-included flags and Lblock state.
    sub_bands: Vec<SubBandEncoderState>,
}

#[derive(Debug, Clone)]
struct SubBandEncoderState {
    geometry: SubBandGeometry,
    inclusion_tree: TagTreeEncoder,
    zero_bitplane_tree: TagTreeEncoder,
    already_included: Vec<bool>,
    lblock: Vec<LblockState>,
}

/// One sub-band's encoder-side leaf data for
/// [`PrecinctEncoderState::new`]: the code-block grid geometry, the
/// per-block first-inclusion layers (§B.10.4 inclusion tag-tree leaves)
/// and the per-block zero-bitplane counts `P` (§B.10.5 tree leaves),
/// both in raster order.
pub type SubBandEncoderPlan = (SubBandGeometry, Vec<u32>, Vec<u32>);

impl PrecinctEncoderState {
    /// Build the per-precinct encoder state.
    ///
    /// `sub_bands` gives, per sub-band in packet order: the code-block
    /// grid geometry, the per-block **first inclusion layer** (leaf
    /// values for the §B.10.4 inclusion tag tree; use a value ≥ the
    /// total layer count for a block never included), and the per-block
    /// zero-bitplane count `P` (leaves for the §B.10.5 tree), both in
    /// raster order.
    ///
    /// Panics if a leaf slice's length disagrees with its geometry.
    pub fn new(sub_bands: &[SubBandEncoderPlan]) -> Self {
        let sb = sub_bands
            .iter()
            .map(|(geom, first_layer, zbp)| {
                let n = (geom.width as usize) * (geom.height as usize);
                assert_eq!(first_layer.len(), n);
                assert_eq!(zbp.len(), n);
                SubBandEncoderState {
                    geometry: *geom,
                    inclusion_tree: TagTreeEncoder::new(geom.width, geom.height, first_layer),
                    zero_bitplane_tree: TagTreeEncoder::new(geom.width, geom.height, zbp),
                    already_included: vec![false; n],
                    lblock: vec![LblockState::default(); n],
                }
            })
            .collect();
        PrecinctEncoderState { sub_bands: sb }
    }
}

/// Encode one packet header (T.800 §B.10.8, `SegmentSplit::Single`
/// layout, no SOP / EPH framing) — the write-side counterpart of
/// [`decode_packet_header`].
///
/// `layer` is the packet's 0-based layer index; `plans` lists one
/// [`CodeBlockPlan`] per code-block in the §B.10.8 order (each sub-band
/// of `state`, raster order within the sub-band). An all-`included:
/// false` plan list produces the §B.10.3 one-bit empty-packet header.
/// Returns the byte-aligned header; the caller appends each included
/// block's codeword-segment bytes (in the same order) as the packet
/// body.
///
/// Panics if `plans.len()` disagrees with the state's total code-block
/// count.
pub fn encode_packet_header(
    state: &mut PrecinctEncoderState,
    layer: u16,
    plans: &[CodeBlockPlan],
) -> Vec<u8> {
    let total: usize = state
        .sub_bands
        .iter()
        .map(|s| (s.geometry.width as usize) * (s.geometry.height as usize))
        .sum();
    assert_eq!(plans.len(), total, "one plan per code-block required");

    let mut writer = PacketBitWriter::new();
    if plans.iter().all(|p| !p.included) {
        // §B.10.3: empty packet — a single 0 bit, then byte-align.
        writer.write_bit(0);
        return writer.finish();
    }
    writer.write_bit(1);

    let mut plan_idx = 0usize;
    for sb in &mut state.sub_bands {
        if sb.geometry.width == 0 || sb.geometry.height == 0 {
            continue;
        }
        for y in 0..sb.geometry.height {
            for x in 0..sb.geometry.width {
                let plan = &plans[plan_idx];
                plan_idx += 1;
                let idx = (y as usize) * (sb.geometry.width as usize) + (x as usize);
                if sb.already_included[idx] {
                    // §B.10.4: one bit — included this layer or not.
                    writer.write_bit(plan.included as u8);
                } else {
                    // §B.10.4: inclusion tag tree at threshold layer+1.
                    sb.inclusion_tree
                        .encode_below_threshold(x, y, (layer as u32) + 1, &mut writer);
                    if plan.included {
                        // §B.10.5: first inclusion — commit the
                        // zero-bitplane leaf value.
                        sb.zero_bitplane_tree.encode_value(x, y, &mut writer);
                    }
                }
                if !plan.included {
                    continue;
                }
                sb.already_included[idx] = true;
                encode_coding_passes(plan.coding_passes, &mut writer);
                encode_segment_lengths(&mut sb.lblock[idx], &plan.segments, &mut writer);
            }
        }
    }
    writer.finish()
}

// ---------------------------------------------------------------------------
// Tests — synthetic bit-stuffed buffers built from T.800 §B.10.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- §D.6 / Table D.9 bypass segment split --------------------------

    #[test]
    fn pass_role_matches_d3_schedule() {
        // Index 0 is the cleanup-only first pass; then SP / MR / cleanup
        // triples repeat (§D.3).
        assert_eq!(pass_role(0), PassRole::Cleanup);
        assert_eq!(pass_role(1), PassRole::Sp);
        assert_eq!(pass_role(2), PassRole::Mr);
        assert_eq!(pass_role(3), PassRole::Cleanup);
        assert_eq!(pass_role(9), PassRole::Cleanup); // fourth cleanup
        assert_eq!(pass_role(10), PassRole::Sp); // first raw pass
        assert_eq!(pass_role(11), PassRole::Mr);
        assert_eq!(pass_role(12), PassRole::Cleanup);
    }

    #[test]
    fn bypass_termination_follows_table_d9() {
        // No bit-2: only the fourth cleanup (i=9) and, from bit-plane 5
        // (i>=10), every MR raw and cleanup AC pass terminate; the SP raw
        // pass does not.
        for i in 0..9 {
            assert!(
                !bypass_pass_terminated(i, false),
                "AC pass {i} not terminated"
            );
        }
        assert!(bypass_pass_terminated(9, false)); // fourth cleanup
        assert!(!bypass_pass_terminated(10, false)); // SP raw — not terminated
        assert!(bypass_pass_terminated(11, false)); // MR raw — terminated
        assert!(bypass_pass_terminated(12, false)); // cleanup AC — terminated
        assert!(!bypass_pass_terminated(13, false)); // SP raw — not terminated
                                                     // Bit-2 set → every pass terminated (including both raw passes).
        for i in 0..14 {
            assert!(
                bypass_pass_terminated(i, true),
                "pass {i} terminated under bit-2"
            );
        }
    }

    #[test]
    fn bypass_is_raw_only_sp_mr_from_bitplane_five() {
        assert!(!bypass_pass_is_raw(0));
        assert!(!bypass_pass_is_raw(9)); // fourth cleanup — AC
        assert!(bypass_pass_is_raw(10)); // SP raw
        assert!(bypass_pass_is_raw(11)); // MR raw
        assert!(!bypass_pass_is_raw(12)); // cleanup — always AC
        assert!(bypass_pass_is_raw(13)); // SP raw
    }

    // -- T.814 §B.2 HT codeword-segment split ---------------------------

    #[test]
    fn ht_boundary_set_matches_t814_b2() {
        // T = ⋃ₖ ⌈3k/2⌉ = {0, 2, 3, 5, 6, 8, 9, 11, 12, …}: every index
        // except the SigProp passes (i ≡ 1 mod 3).
        let expect: Vec<u32> = vec![0, 2, 3, 5, 6, 8, 9, 11, 12];
        let got: Vec<u32> = (0..=12).filter(|&i| ht_pass_ends_segment(i, 0)).collect();
        assert_eq!(got, expect);
        // Cross-check against the closed form ⌈3k/2⌉ directly.
        let closed: Vec<u32> = (0..9u32).map(|k| (3 * k).div_ceil(2)).collect();
        assert_eq!(got, closed);
        // With 3·P0 placeholder passes the whole set shifts by 3P0 and
        // no boundary falls inside the placeholder run (§B.2).
        let shifted: Vec<u32> = (0..=15).filter(|&i| ht_pass_ends_segment(i, 3)).collect();
        let expect3: Vec<u32> = expect.iter().map(|&i| i + 3).collect();
        assert_eq!(shifted, expect3);
    }

    #[test]
    fn ht_segment_spans_shapes() {
        // Z_blk = 1: the cleanup pass alone (§B.3 NOTE 1).
        assert_eq!(ht_segment_spans(0, 1, 0), vec![1]);
        // Z_blk = 2: cleanup, then a SigProp-only refinement segment.
        assert_eq!(ht_segment_spans(0, 2, 0), vec![1, 1]);
        // Z_blk = 3: cleanup, then SigProp + MagRef in one segment.
        assert_eq!(ht_segment_spans(0, 3, 0), vec![1, 2]);
        // Refinement passes arriving in a later packet (layered): the
        // cursor starts past the cleanup.
        assert_eq!(ht_segment_spans(1, 2, 0), vec![2]);
        assert_eq!(ht_segment_spans(2, 1, 0), vec![1]);
        // A second HT set repeats the pattern (MULTIHT shape).
        assert_eq!(ht_segment_spans(0, 6, 0), vec![1, 2, 1, 2]);
        assert_eq!(ht_segment_spans(3, 3, 0), vec![1, 2]);
        // Empty contribution.
        assert!(ht_segment_spans(4, 0, 0).is_empty());
    }

    #[test]
    fn ht_segment_spans_with_placeholders() {
        // One placeholder triple (P0 = 1): the first HT cleanup pass
        // sits at absolute index 3 and the set-T boundaries shift by
        // 3·P0 (§B.2). A contribution carrying the whole placeholder
        // run plus the first HT set folds the placeholders into the
        // cleanup segment (§B.1 — they contribute no bytes).
        assert_eq!(ht_segment_spans(0, 6, 3), vec![4, 2]);
        // The placeholder run alone has no set-T boundary — one
        // segment ending at the last included pass.
        assert_eq!(ht_segment_spans(0, 3, 3), vec![3]);
        // The first HT set arriving after the placeholder packet.
        assert_eq!(ht_segment_spans(3, 3, 3), vec![1, 2]);
        // A second HT set under P0 = 1 (MULTIHT + placeholders).
        assert_eq!(ht_segment_spans(6, 3, 3), vec![1, 2]);
        // Mid-triple placeholder split across packets: the second
        // chunk continues the run and then carries the cleanup pass.
        assert_eq!(ht_segment_spans(0, 2, 3), vec![2]);
        assert_eq!(ht_segment_spans(2, 2, 3), vec![2]);
    }

    #[test]
    fn ht_first_cleanup_candidate_pins_p0() {
        // First contribution shapes: the candidate is the last
        // multiple of 3 inside the contribution.
        assert_eq!(ht_first_cleanup_candidate(0, 1), Some(0));
        assert_eq!(ht_first_cleanup_candidate(0, 2), Some(0));
        assert_eq!(ht_first_cleanup_candidate(0, 3), Some(0));
        // A placeholder triple followed by the first HT set.
        assert_eq!(ht_first_cleanup_candidate(3, 3), Some(3));
        assert_eq!(ht_first_cleanup_candidate(0, 6), Some(3));
        // Placeholders + cleanup (+ SigProp) inside one contribution.
        assert_eq!(ht_first_cleanup_candidate(0, 4), Some(3));
        assert_eq!(ht_first_cleanup_candidate(0, 5), Some(3));
        // No multiple of 3 inside the span: placeholder-run extension
        // only.
        assert_eq!(ht_first_cleanup_candidate(4, 1), None);
        assert_eq!(ht_first_cleanup_candidate(4, 2), None);
        assert_eq!(ht_first_cleanup_candidate(7, 1), None);
        assert_eq!(ht_first_cleanup_candidate(0, 0), None);
    }

    /// MULTIHT + placeholder packet headers round-trip through the
    /// writer / reader pair: the reader pins `3·P0` from the first
    /// non-zero length field (§B.3) and re-derives the same set-`T`
    /// spans — including the trial read whose first field is narrower
    /// than a placeholder run's (Equation B-19).
    #[test]
    fn packet_header_ht_placeholders_and_sets_round_trip() {
        let geom = SubBandGeometry {
            width: 2,
            height: 1,
        };
        // Block 0 spends layer 0 on a placeholder triple (P0 = 1);
        // block 1 starts its first HT set immediately. Both then carry
        // one HT set per layer (zero-length refinement on non-final
        // sets, §B.3 NOTE 3).
        type Shape = (u32, Vec<(u32, u32)>);
        let shapes: [[Shape; 2]; 3] = [
            [(3, vec![(3, 0)]), (3, vec![(1, 7), (2, 0)])],
            [(3, vec![(1, 5), (2, 0)]), (3, vec![(1, 9), (2, 4)])],
            [(1, vec![(1, 11)]), (1, vec![(1, 6)])],
        ];
        let zbp = vec![1u32, 0];
        let mut enc = PrecinctEncoderState::new(&[(geom, vec![0u32, 0], zbp.clone())]);
        let headers: Vec<Vec<u8>> = shapes
            .iter()
            .enumerate()
            .map(|(l, row)| {
                let plans: Vec<CodeBlockPlan> = row
                    .iter()
                    .zip(zbp.iter())
                    .map(|((passes, segs), &p)| CodeBlockPlan {
                        included: true,
                        zero_bit_planes: p,
                        coding_passes: *passes,
                        segments: segs.clone(),
                    })
                    .collect();
                encode_packet_header(&mut enc, l as u16, &plans)
            })
            .collect();

        let mut dec = PrecinctState::new();
        for (l, hb) in headers.iter().enumerate() {
            let geometry = PacketGeometry {
                sub_bands: vec![geom],
                layer: l as u16,
            };
            let hdr =
                decode_packet_header(hb, &geometry, &mut dec, SopEphMode::None, SegmentSplit::Ht)
                    .unwrap();
            for (b, c) in hdr.contributions.iter().enumerate() {
                assert!(c.included, "layer {l} block {b}");
                assert_eq!(c.coding_passes, shapes[l][b].0, "layer {l} block {b}");
                let want: Vec<u32> = shapes[l][b].1.iter().map(|s| s.1).collect();
                assert_eq!(c.segment_lengths, want, "layer {l} block {b}");
            }
        }
        // The placeholder count resolved where the first cleanup
        // segment appeared: pass 3 for block 0, pass 0 for block 1.
        assert_eq!(
            dec.sub_bands[0].ht_placeholder_passes,
            vec![Some(3), Some(0)]
        );
    }

    /// A bit-plane-skip HT set (§B.3 NOTE 2: zero-length cleanup +
    /// refinement segments) and a refinement segment split across
    /// packets (§B.3: SigProp and MagRef may land in different layers)
    /// both round-trip.
    #[test]
    fn packet_header_ht_skip_set_and_split_refinement_round_trip() {
        let geom = SubBandGeometry {
            width: 1,
            height: 1,
        };
        // L0: set-0 cleanup; L1: set-0 SigProp alone; L2: set-0 MagRef
        // alone; L3: a whole skip set (three passes, both segments
        // zero-length); L4: set-2 cleanup.
        type Shape = (u32, Vec<(u32, u32)>);
        let shapes: [Shape; 5] = [
            (1, vec![(1, 8)]),
            (1, vec![(1, 3)]),
            (1, vec![(1, 2)]),
            (3, vec![(1, 0), (2, 0)]),
            (1, vec![(1, 13)]),
        ];
        let mut enc = PrecinctEncoderState::new(&[(geom, vec![0u32], vec![4u32])]);
        let headers: Vec<Vec<u8>> = shapes
            .iter()
            .enumerate()
            .map(|(l, (passes, segs))| {
                encode_packet_header(
                    &mut enc,
                    l as u16,
                    &[CodeBlockPlan {
                        included: true,
                        zero_bit_planes: 4,
                        coding_passes: *passes,
                        segments: segs.clone(),
                    }],
                )
            })
            .collect();
        let mut dec = PrecinctState::new();
        for (l, hb) in headers.iter().enumerate() {
            let geometry = PacketGeometry {
                sub_bands: vec![geom],
                layer: l as u16,
            };
            let hdr =
                decode_packet_header(hb, &geometry, &mut dec, SopEphMode::None, SegmentSplit::Ht)
                    .unwrap();
            let c = &hdr.contributions[0];
            assert_eq!(c.coding_passes, shapes[l].0, "layer {l}");
            let want: Vec<u32> = shapes[l].1.iter().map(|s| s.1).collect();
            assert_eq!(c.segment_lengths, want, "layer {l}");
        }
    }

    /// A placeholder run whose first length field is non-zero is not a
    /// legal reading under either §B.3 interpretation (a first HT
    /// cleanup segment of length 1 is barred, and a placeholder run
    /// carries no bytes) — the reader rejects rather than desyncs.
    #[test]
    fn packet_header_ht_length_one_first_segment_rejected() {
        let geom = SubBandGeometry {
            width: 1,
            height: 1,
        };
        // Write a contribution shaped like a first HT set whose
        // cleanup length is 1.
        let mut enc = PrecinctEncoderState::new(&[(geom, vec![0u32], vec![0u32])]);
        let hb = encode_packet_header(
            &mut enc,
            0,
            &[CodeBlockPlan {
                included: true,
                zero_bit_planes: 0,
                coding_passes: 3,
                segments: vec![(1, 1), (2, 0)],
            }],
        );
        let geometry = PacketGeometry {
            sub_bands: vec![geom],
            layer: 0,
        };
        let mut dec = PrecinctState::new();
        assert!(
            decode_packet_header(&hb, &geometry, &mut dec, SopEphMode::None, SegmentSplit::Ht,)
                .is_err()
        );
    }

    #[test]
    fn bypass_segment_spans_default_schedule() {
        // 14 passes from a fresh code-block: the AC region (10 passes,
        // ending at the fourth cleanup) is one segment; then each
        // bit-plane-5+ set splits into {SP raw, MR raw} (terminates on
        // the MR) and {cleanup AC}.
        let spans = bypass_segment_spans(0, 14, false);
        assert_eq!(
            spans,
            vec![
                (10, false), // passes 0..=9 AC, terminate on fourth cleanup
                (2, true),   // passes 10,11 = SP raw + MR raw (terminate)
                (1, false),  // pass 12 = cleanup AC (terminate)
                (1, true),   // pass 13 = SP raw (final included pass → in T)
            ]
        );
        // Total passes across spans equals the contribution.
        assert_eq!(spans.iter().map(|(p, _)| p).sum::<u32>(), 14);
    }

    #[test]
    fn bypass_segment_spans_short_block_single_ac_segment() {
        // Fewer than 10 passes never reach the raw region — one AC
        // segment carries every pass (the final-pass rule adds it to T).
        let spans = bypass_segment_spans(0, 7, false);
        assert_eq!(spans, vec![(7, false)]);
    }

    #[test]
    fn bypass_segment_spans_termall_one_per_pass() {
        // Bit-2 also set → every pass terminated, including both raw
        // passes; each span is a single pass and carries its raw / AC tag.
        let spans = bypass_segment_spans(0, 13, true);
        assert_eq!(spans.len(), 13);
        // Index 10 = SP raw, 11 = MR raw, 12 = cleanup AC.
        assert_eq!(spans[10], (1, true));
        assert_eq!(spans[11], (1, true));
        assert_eq!(spans[12], (1, false));
    }

    #[test]
    fn bypass_segment_spans_resume_across_layers() {
        // A second packet resuming at absolute pass index 10 starts in
        // the raw region: {SP raw, MR raw} then {cleanup AC}.
        let spans = bypass_segment_spans(10, 3, false);
        assert_eq!(spans, vec![(2, true), (1, false)]);
    }

    /// Pack a sequence of bits (MSB-first) into a `Vec<u8>` per T.800
    /// §B.10.1: after every `0xFF` byte produced, the **next** bit
    /// written is preceded by a stuffed zero bit (so 7 of the next
    /// byte's 8 bits carry payload).
    fn pack_bits(bits: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cur: u8 = 0;
        let mut bits_used: u8 = 0;
        let mut prev_was_ff = false;
        for &bit in bits {
            // If the previous byte was 0xFF, insert a stuffed 0 bit
            // before writing this one.
            if prev_was_ff && bits_used == 0 {
                cur = 0;
                bits_used = 1; // top bit of `cur` is the stuffed 0
            }
            cur = (cur << 1) | (bit & 1);
            bits_used += 1;
            if bits_used == 8 {
                out.push(cur);
                prev_was_ff = cur == 0xFF;
                cur = 0;
                bits_used = 0;
            }
        }
        // Pad final byte to a byte boundary per §B.10.1.
        if bits_used != 0 {
            cur <<= 8 - bits_used;
            out.push(cur);
        }
        out
    }

    #[test]
    fn bit_reader_reads_msb_first() {
        // 0xA5 = 1010 0101.
        let mut r = PacketBitReader::new(&[0xA5]);
        let expected = [1, 0, 1, 0, 0, 1, 0, 1];
        for e in expected {
            assert_eq!(r.read_bit().unwrap(), e);
        }
        assert!(r.read_bit().is_err());
    }

    #[test]
    fn bit_reader_skips_stuffed_zero_after_ff() {
        // First byte 0xFF (all ones), second byte 0xA5 — but per
        // §B.10.1 the second byte's MSB is the stuffed zero, so the
        // payload is the **remaining 7 bits** of 0xA5 = 010 0101.
        let mut r = PacketBitReader::new(&[0xFF, 0xA5]);
        // First 8 bits = 8 ones.
        for _ in 0..8 {
            assert_eq!(r.read_bit().unwrap(), 1);
        }
        // Next: skip stuffed zero, then read 0xA5 << 1's top 7 bits:
        // 0xA5 = 1010 0101 → << 1 = 0100 1010 → top 7 bits are 010 0101.
        let next_seven = [0, 1, 0, 0, 1, 0, 1];
        for e in next_seven {
            assert_eq!(r.read_bit().unwrap(), e);
        }
    }

    #[test]
    fn bit_reader_packs_and_unpacks_roundtrip() {
        // A bit pattern that produces an 0xFF byte mid-stream:
        // 1111 1111 0 1 1 → byte 0xFF, then byte starts with stuffed
        // 0 then payload 011 + padding.
        let bits = [1, 1, 1, 1, 1, 1, 1, 1, 0, 1, 1];
        let packed = pack_bits(&bits);
        assert_eq!(packed[0], 0xFF);
        let mut r = PacketBitReader::new(&packed);
        for e in bits {
            assert_eq!(r.read_bit().unwrap(), e);
        }
    }

    // -----------------------------------------------------------------------
    // Tag tree tests against the worked example in T.800 §B.10.2 NOTE.
    // -----------------------------------------------------------------------

    #[test]
    fn tag_tree_decode_full_value() {
        // Build a 1×1 tag tree (depth 1, single leaf). decode_value
        // should read 0s until it sees a 1, returning the count of 0s.
        let mut t = TagTree::new(1, 1);
        let bits = pack_bits(&[0, 0, 1]); // value = 2
        let mut r = PacketBitReader::new(&bits);
        assert_eq!(t.decode_value(0, 0, &mut r).unwrap(), 2);
    }

    #[test]
    fn tag_tree_decode_below_threshold_partial() {
        // 1×1 tree: read 0, 0, 0 — three "value is greater than
        // current" responses — without a closing 1 bit. With threshold
        // = 3 we should stop after the third 0 and return false (leaf
        // value is >= 3 but not yet known).
        let mut t = TagTree::new(1, 1);
        let bits = pack_bits(&[0, 0, 0]);
        let mut r = PacketBitReader::new(&bits);
        let is_below = t.decode_below_threshold(0, 0, 3, &mut r).unwrap();
        assert!(!is_below);
    }

    #[test]
    fn tag_tree_decode_below_threshold_true() {
        // 1×1 tree: read 1 — leaf is 0, which is < 1 threshold.
        let mut t = TagTree::new(1, 1);
        let bits = pack_bits(&[1]);
        let mut r = PacketBitReader::new(&bits);
        let is_below = t.decode_below_threshold(0, 0, 1, &mut r).unwrap();
        assert!(is_below);
    }

    #[test]
    fn tag_tree_remembers_state_across_calls() {
        // 1×1 tree. First call asks "below threshold 2?" with bits
        // [0, 1] — leaf value is 1, true. Second call on the same
        // leaf with threshold 3 should NOT consume any more bits
        // (value already committed at 1).
        let mut t = TagTree::new(1, 1);
        let bits = pack_bits(&[0, 1]);
        let mut r = PacketBitReader::new(&bits);
        let below_2 = t.decode_below_threshold(0, 0, 2, &mut r).unwrap();
        assert!(below_2);
        // No more bits needed: subsequent threshold-3 query terminates
        // immediately (we don't read past the committed value).
        let below_3 = t.decode_below_threshold(0, 0, 3, &mut r).unwrap();
        assert!(below_3);
    }

    #[test]
    fn tag_tree_2x2_decode_value() {
        // 2×2 tree, depth 2: root at level 0 (1 node), leaves at
        // level 1 (4 nodes). decode_value at (0, 0) reads:
        //   level 0: 0, 0, 1 → root = 2
        //   level 1 (at 0,0): 1 → leaf already at 2, no increment
        // Then check that decode_value at (1, 0) does NOT re-read the
        // level-0 bits.
        let mut t = TagTree::new(2, 2);
        let bits = pack_bits(&[0, 0, 1, 1, 0, 1]);
        // First three bits drive the root to 2 and decode it.
        // Fourth bit decodes the (0,0) leaf at value 2.
        // Next bits cover (1,0): root already known at 2; level-1
        // node initialised at 2. 0 → 3, 1 → committed at 3.
        let mut r = PacketBitReader::new(&bits);
        let v00 = t.decode_value(0, 0, &mut r).unwrap();
        assert_eq!(v00, 2);
        let v10 = t.decode_value(1, 0, &mut r).unwrap();
        assert_eq!(v10, 3);
    }

    // -----------------------------------------------------------------------
    // Coding-passes Huffman — T.800 Table B.4.
    // -----------------------------------------------------------------------

    #[test]
    fn coding_passes_1_through_5() {
        for (bits, expected) in [
            (vec![0u8], 1),
            (vec![1, 0], 2),
            (vec![1, 1, 0, 0], 3),
            (vec![1, 1, 0, 1], 4),
            (vec![1, 1, 1, 0], 5),
        ] {
            let packed = pack_bits(&bits);
            let mut r = PacketBitReader::new(&packed);
            assert_eq!(decode_coding_passes(&mut r).unwrap(), expected);
        }
    }

    #[test]
    fn coding_passes_6_through_36() {
        // Encodes value 6..36 as prefix 1111 + 5-bit a where a = value - 6.
        for value in [6u32, 7, 20, 35, 36] {
            let a = value - 6;
            let mut bits = vec![1u8, 1, 1, 1];
            for shift in (0..5).rev() {
                bits.push(((a >> shift) & 1) as u8);
            }
            let packed = pack_bits(&bits);
            let mut r = PacketBitReader::new(&packed);
            assert_eq!(decode_coding_passes(&mut r).unwrap(), value);
        }
    }

    #[test]
    fn coding_passes_37_through_164() {
        // Encodes value 37..164 as prefix 1111 + 11111 (a=31) + 7-bit b
        // where b = value - 37.
        for value in [37u32, 38, 100, 163, 164] {
            let b = value - 37;
            let mut bits = vec![1u8, 1, 1, 1, 1, 1, 1, 1, 1];
            for shift in (0..7).rev() {
                bits.push(((b >> shift) & 1) as u8);
            }
            let packed = pack_bits(&bits);
            let mut r = PacketBitReader::new(&packed);
            assert_eq!(decode_coding_passes(&mut r).unwrap(), value);
        }
    }

    // -----------------------------------------------------------------------
    // Lblock-based segment length — T.800 §B.10.7.1.
    // -----------------------------------------------------------------------

    #[test]
    fn segment_length_initial_lblock_no_increment() {
        // Lblock initial = 3. 1 coding pass → log2(1) = 0 extra bits;
        // total = 3 bits. Encode (length = 5) as `0` (no increment) +
        // 3 bits 101.
        let bits = vec![0u8, 1, 0, 1];
        let packed = pack_bits(&bits);
        let mut r = PacketBitReader::new(&packed);
        let mut st = LblockState::default();
        let len = decode_segment_length(&mut st, 1, &mut r).unwrap();
        assert_eq!(len, 5);
        assert_eq!(st.lblock, 3);
    }

    #[test]
    fn segment_length_with_lblock_increment() {
        // Lblock initial = 3. With 1 pass we'd use 3 bits; bump
        // Lblock by 2 (bits `110` + terminator `0`) to use 5 bits.
        // Then encode length 17 as 5 bits = 10001.
        let bits = vec![1u8, 1, 0, 1, 0, 0, 0, 1];
        let packed = pack_bits(&bits);
        let mut r = PacketBitReader::new(&packed);
        let mut st = LblockState::default();
        let len = decode_segment_length(&mut st, 1, &mut r).unwrap();
        assert_eq!(len, 17);
        assert_eq!(st.lblock, 5);
    }

    #[test]
    fn segment_length_with_multiple_passes_extra_bits() {
        // Lblock = 3, passes = 3 → floor(log2 3) = 1 → 4 bits used.
        // No Lblock increment. Encode length = 12 as 4 bits 1100.
        let bits = vec![0u8, 1, 1, 0, 0];
        let packed = pack_bits(&bits);
        let mut r = PacketBitReader::new(&packed);
        let mut st = LblockState::default();
        let len = decode_segment_length(&mut st, 3, &mut r).unwrap();
        assert_eq!(len, 12);
        assert_eq!(st.lblock, 3);
    }

    // -----------------------------------------------------------------------
    // Full packet-header tests.
    // -----------------------------------------------------------------------

    #[test]
    fn empty_packet_consumes_one_byte() {
        // Zero-length bit = 0; then byte-align → 1 byte consumed.
        let bits = vec![0u8];
        let packed = pack_bits(&bits);
        let mut state = PrecinctState::new();
        let geom = PacketGeometry {
            sub_bands: vec![SubBandGeometry {
                width: 1,
                height: 1,
            }],
            layer: 0,
        };
        let h = decode_packet_header(
            &packed,
            &geom,
            &mut state,
            SopEphMode::None,
            SegmentSplit::Single,
        )
        .unwrap();
        assert!(!h.non_zero_length);
        assert!(h.contributions.is_empty());
        assert_eq!(h.bytes_consumed, packed.len());
        assert_eq!(h.total_body_bytes(), 0);
    }

    #[test]
    fn single_codeblock_first_inclusion() {
        // Geometry: 1 sub-band, 1 × 1 code-block grid, layer 0.
        // Bits in the packet header (T.800 §B.10.8 order):
        //   1                — non-zero packet
        //   1                — inclusion tag tree query at threshold 1
        //                      (leaf value 0 < 1 → included; depth 1
        //                      tree just reads a single 1)
        //   1                — zero-bitplane tree value = 0 (single 1)
        //   0                — coding passes = 1 (`0` codeword)
        //   0                — no Lblock increment
        //   ddd              — 3-bit length (lblock=3 + log2(1)=0)
        //
        // Pick length = 5 → bits `101`. So total = 1 1 1 0 0 101.
        let bits = vec![1u8, 1, 1, 0, 0, 1, 0, 1];
        let packed = pack_bits(&bits);
        let geom = PacketGeometry {
            sub_bands: vec![SubBandGeometry {
                width: 1,
                height: 1,
            }],
            layer: 0,
        };
        let mut state = PrecinctState::new();
        let h = decode_packet_header(
            &packed,
            &geom,
            &mut state,
            SopEphMode::None,
            SegmentSplit::Single,
        )
        .unwrap();
        assert!(h.non_zero_length);
        assert_eq!(h.contributions.len(), 1);
        let c = &h.contributions[0];
        assert!(c.included);
        assert_eq!(c.coding_passes, 1);
        assert_eq!(c.segment_lengths, vec![5]);
        assert_eq!(c.zero_bit_planes, Some(0));
        assert_eq!(h.total_body_bytes(), 5);
        // Should have flagged this code-block as already included for
        // subsequent layers.
        assert!(state.sub_bands[0].already_included[0]);
    }

    #[test]
    fn per_pass_split_reads_one_length_per_pass() {
        // §B.10.7.2 termination-on-each-pass: a contribution with K
        // coding passes signals K codeword-segment lengths, with the
        // increase-Lblock prefix appearing **once** before the first
        // length (per the §B.10.7.2 worked example "the value of Lblock
        // is incremented only at the start of the sequence"). Each
        // length is `lblock` bits (passes_in_segment = 1 → no widening).
        //
        // Bits (T.800 §B.10.8 order):
        //   1        — non-zero packet
        //   1        — inclusion tag tree (leaf 0 < 1 → included)
        //   1        — zero-bitplane tree value = 0
        //   1100     — coding passes = 3 (Table B.4 codeword)
        //   0        — Lblock increment prefix: no increment (once)
        //   101      — length[0] = 5 (3 bits, lblock = 3)
        //   011      — length[1] = 3
        //   100      — length[2] = 4
        let bits = vec![
            1u8, // non-zero
            1,   // inclusion
            1,   // zero-bitplane = 0
            1, 1, 0, 0, // coding passes = 3
            0, // no Lblock increment (only here, not per length)
            1, 0, 1, // len 5
            0, 1, 1, // len 3
            1, 0, 0, // len 4
        ];
        let packed = pack_bits(&bits);
        let geom = PacketGeometry {
            sub_bands: vec![SubBandGeometry {
                width: 1,
                height: 1,
            }],
            layer: 0,
        };
        let mut state = PrecinctState::new();
        let h = decode_packet_header(
            &packed,
            &geom,
            &mut state,
            SopEphMode::None,
            SegmentSplit::PerPass,
        )
        .unwrap();
        assert!(h.non_zero_length);
        let c = &h.contributions[0];
        assert!(c.included);
        assert_eq!(c.coding_passes, 3);
        // K = 3 lengths, one per terminated pass.
        assert_eq!(c.segment_lengths, vec![5, 3, 4]);
        assert_eq!(c.zero_bit_planes, Some(0));
        assert_eq!(h.total_body_bytes(), 12);
        // Lblock unchanged (the single prefix asked for no increment).
        assert_eq!(state.sub_bands[0].lblock[0].lblock, 3);
    }

    #[test]
    fn per_pass_split_single_prefix_then_widened_lblock() {
        // Same as above but the single increase-Lblock prefix bumps
        // Lblock from 3 to 4 (`10`), so every one of the K = 2 lengths
        // is read with 4 bits — exercising that the prefix applies to
        // all subsequent lengths, not just the first.
        //
        //   1        — non-zero
        //   1        — inclusion
        //   1        — zero-bitplane = 0
        //   1,0      — coding passes = 2
        //   1,0      — Lblock increment: +1 → lblock = 4 (once)
        //   0110     — length[0] = 6 (4 bits)
        //   1001     — length[1] = 9
        let bits = vec![
            1u8, 1, 1, // non-zero, inclusion, zero-bitplane
            1, 0, // coding passes = 2
            1, 0, // Lblock += 1 → 4
            0, 1, 1, 0, // len 6
            1, 0, 0, 1, // len 9
        ];
        let packed = pack_bits(&bits);
        let geom = PacketGeometry {
            sub_bands: vec![SubBandGeometry {
                width: 1,
                height: 1,
            }],
            layer: 0,
        };
        let mut state = PrecinctState::new();
        let h = decode_packet_header(
            &packed,
            &geom,
            &mut state,
            SopEphMode::None,
            SegmentSplit::PerPass,
        )
        .unwrap();
        let c = &h.contributions[0];
        assert_eq!(c.coding_passes, 2);
        assert_eq!(c.segment_lengths, vec![6, 9]);
        assert_eq!(state.sub_bands[0].lblock[0].lblock, 4);
    }

    #[test]
    fn already_included_uses_one_bit_inclusion() {
        // After a first packet that includes the (0,0) block, a second
        // packet's inclusion signalling is a single bit (T.800
        // §B.10.4). Build state directly and feed only the second
        // packet's bits.
        let geom = PacketGeometry {
            sub_bands: vec![SubBandGeometry {
                width: 1,
                height: 1,
            }],
            layer: 1,
        };
        let mut state = PrecinctState::new();
        state.ensure_layout(&geom).unwrap();
        state.sub_bands[0].already_included[0] = true;
        // Bits:
        //   1       — non-zero packet
        //   1       — inclusion bit (included this layer)
        //   0       — coding passes = 1
        //   0       — no Lblock increment
        //   011     — 3-bit length = 3
        let bits = vec![1u8, 1, 0, 0, 0, 1, 1];
        let packed = pack_bits(&bits);
        let h = decode_packet_header(
            &packed,
            &geom,
            &mut state,
            SopEphMode::None,
            SegmentSplit::Single,
        )
        .unwrap();
        assert!(h.non_zero_length);
        let c = &h.contributions[0];
        assert!(c.included);
        assert!(c.zero_bit_planes.is_none()); // not the first inclusion
        assert_eq!(c.coding_passes, 1);
        assert_eq!(c.segment_lengths, vec![3]);
    }

    #[test]
    fn not_yet_included_partial_tag_tree() {
        // Geometry: 1 sub-band, 1×1 grid, layer 0. The tag tree's
        // leaf value will be >= layer+1=1 → not included.
        // Bits:
        //   1       — non-zero packet
        //   0       — inclusion tag tree: leaf value > 0; threshold=1
        //              not reached → not included
        let bits = vec![1u8, 0];
        let packed = pack_bits(&bits);
        let geom = PacketGeometry {
            sub_bands: vec![SubBandGeometry {
                width: 1,
                height: 1,
            }],
            layer: 0,
        };
        let mut state = PrecinctState::new();
        let h = decode_packet_header(
            &packed,
            &geom,
            &mut state,
            SopEphMode::None,
            SegmentSplit::Single,
        )
        .unwrap();
        assert!(h.non_zero_length);
        let c = &h.contributions[0];
        assert!(!c.included);
        assert_eq!(c.segment_lengths, Vec::<u32>::new());
        assert_eq!(c.coding_passes, 0);
        // Not yet included → already_included still false.
        assert!(!state.sub_bands[0].already_included[0]);
    }

    #[test]
    fn walk_two_packets_same_precinct_inclusion_persists() {
        // Two packets, both for the same precinct, layer 0 then
        // layer 1. The first includes the block; the second uses the
        // one-bit inclusion signalling.
        // Packet 0 bits: 1 1 1 0 0 101 (8 bits → 1 byte)
        // Packet 1 bits: 1 1 0 0 010 (7 bits → 1 byte with pad)
        let mut all = Vec::new();
        all.extend(&pack_bits(&[1, 1, 1, 0, 0, 1, 0, 1]));
        // 5 body bytes of the first packet
        all.extend([0u8; 5]);
        all.extend(&pack_bits(&[1, 1, 0, 0, 0, 1, 0]));
        // 2 body bytes of the second packet
        all.extend([0u8; 2]);

        let g0 = PacketGeometry {
            sub_bands: vec![SubBandGeometry {
                width: 1,
                height: 1,
            }],
            layer: 0,
        };
        let g1 = PacketGeometry {
            sub_bands: vec![SubBandGeometry {
                width: 1,
                height: 1,
            }],
            layer: 1,
        };
        let headers = walk_packet_headers(
            &all,
            &[
                (0usize, g0.clone(), SegmentSplit::Single),
                (0usize, g1.clone(), SegmentSplit::Single),
            ],
            SopEphMode::None,
        )
        .unwrap();
        assert_eq!(headers.len(), 2);
        assert!(headers[0].non_zero_length);
        assert!(headers[1].non_zero_length);
        assert_eq!(headers[0].contributions[0].segment_lengths, vec![5]);
        assert_eq!(headers[1].contributions[0].segment_lengths, vec![2]);
        // The second packet didn't read a zero-bitplane field — its
        // contribution's zero_bit_planes is None.
        assert!(headers[1].contributions[0].zero_bit_planes.is_none());
    }

    // -- Relocated-header (PPM / PPT) walker, T.800 §A.7.4 / §A.7.5 ------

    /// The relocated walker must recover exactly the same packet headers
    /// and code-block contributions as the in-stream walker when the same
    /// stream is split into separate header / data buffers.
    ///
    /// Build the two-packet, single-precinct, two-layer stream from
    /// `walk_two_packets_same_precinct_inclusion_persists`, then peel its
    /// byte-aligned packet headers off into one buffer and the code-block
    /// data into another — exactly the relocation PPT performs — and
    /// assert the relocated walk reproduces the inline result byte-for-
    /// byte, with body offsets pointing at the right data spans.
    #[test]
    fn separate_walk_matches_inline_two_packets() {
        let h0 = pack_bits(&[1, 1, 1, 0, 0, 1, 0, 1]); // packet-0 header
        let d0 = [0xAAu8; 5]; // packet-0 data (length 5)
        let h1 = pack_bits(&[1, 1, 0, 0, 0, 1, 0]); // packet-1 header
        let d1 = [0xBBu8; 2]; // packet-1 data (length 2)

        // In-stream layout: header then data, interleaved.
        let mut inline = Vec::new();
        inline.extend_from_slice(&h0);
        inline.extend_from_slice(&d0);
        inline.extend_from_slice(&h1);
        inline.extend_from_slice(&d1);

        // Relocated layout: all headers concatenated; all data
        // concatenated; two distinct buffers.
        let mut header_buf = Vec::new();
        header_buf.extend_from_slice(&h0);
        header_buf.extend_from_slice(&h1);
        let mut data_buf = Vec::new();
        data_buf.extend_from_slice(&d0);
        data_buf.extend_from_slice(&d1);

        let g = |layer| PacketGeometry {
            sub_bands: vec![SubBandGeometry {
                width: 1,
                height: 1,
            }],
            layer,
        };
        let packets = [
            (0usize, g(0), SegmentSplit::Single),
            (0usize, g(1), SegmentSplit::Single),
        ];

        let inline_headers = walk_packet_headers(&inline, &packets, SopEphMode::None).unwrap();
        let relocated =
            walk_packet_headers_separate(&header_buf, &data_buf, &packets, SopEphMode::None)
                .unwrap();

        assert_eq!(relocated.len(), inline_headers.len());
        // Body offsets address the two data spans in `data_buf`.
        assert_eq!(relocated[0].body_offset, 0);
        assert_eq!(relocated[1].body_offset, 5);
        for (rp, ih) in relocated.iter().zip(inline_headers.iter()) {
            assert_eq!(rp.header.non_zero_length, ih.non_zero_length);
            assert_eq!(rp.header.contributions.len(), ih.contributions.len());
            for (rc, ic) in rp.header.contributions.iter().zip(ih.contributions.iter()) {
                assert_eq!(rc.included, ic.included);
                assert_eq!(rc.segment_lengths, ic.segment_lengths);
                assert_eq!(rc.coding_passes, ic.coding_passes);
                assert_eq!(rc.zero_bit_planes, ic.zero_bit_planes);
            }
        }
    }

    /// With EPH framing the relocated walk consumes the trailing EPH
    /// from the **header** buffer (§A.8.2) and an SOP from the **body**
    /// buffer (§A.8.1) — the two markers live in different streams once
    /// the headers are moved into PPT.
    #[test]
    fn separate_walk_sop_in_body_eph_in_header() {
        // One non-empty packet, single 1×1 block, length-4 segment.
        let header_bits = pack_bits(&[1, 1, 1, 0, 0, 1, 0, 0]); // length 4
        let mut header_buf = header_bits;
        header_buf.extend_from_slice(&[0xFF, 0x92]); // EPH after the header

        // Body: SOP (Nsop = 0) then the 4 data bytes.
        let mut data_buf = vec![0xFFu8, 0x91, 0x00, 0x04, 0x00, 0x00];
        data_buf.extend_from_slice(&[0xCCu8; 4]);

        let geom = PacketGeometry {
            sub_bands: vec![SubBandGeometry {
                width: 1,
                height: 1,
            }],
            layer: 0,
        };
        let relocated = walk_packet_headers_separate(
            &header_buf,
            &data_buf,
            &[(0usize, geom, SegmentSplit::Single)],
            SopEphMode::SopAndEph,
        )
        .unwrap();
        assert_eq!(relocated.len(), 1);
        // Data begins after the 6-byte SOP in the body.
        assert_eq!(relocated[0].body_offset, 6);
        assert!(relocated[0].header.non_zero_length);
        assert_eq!(
            relocated[0].header.contributions[0].segment_lengths,
            vec![4]
        );
    }

    /// A body SOP whose Nsop does not match the running packet ordinal
    /// flags a lost / mis-ordered packet (§A.8.1) even when the headers
    /// are relocated.
    #[test]
    fn separate_walk_rejects_wrong_body_nsop() {
        let header_buf = pack_bits(&[0]); // single empty packet
                                          // SOP carrying Nsop = 7 for what should be packet 0.
        let data_buf = vec![0xFFu8, 0x91, 0x00, 0x04, 0x00, 0x07];
        let geom = PacketGeometry {
            sub_bands: vec![SubBandGeometry {
                width: 1,
                height: 1,
            }],
            layer: 0,
        };
        let err = walk_packet_headers_separate(
            &header_buf,
            &data_buf,
            &[(0usize, geom, SegmentSplit::Single)],
            SopEphMode::SopAndEph,
        )
        .unwrap_err();
        assert_eq!(err, Error::InvalidPacketHeader);
    }

    #[test]
    fn walk_rejects_overrun_against_short_body() {
        // Geometry promises one packet whose body is 100 bytes, but
        // the buffer only has 8 bytes total (header + small body).
        let bits = vec![1u8, 1, 1, 0, 0, 1, 0, 1]; // length-5 length field; body=5
        let mut all = pack_bits(&bits);
        // No body bytes appended → expect overrun.
        all.truncate(all.len()); // (no-op; just being explicit)
        let geom = PacketGeometry {
            sub_bands: vec![SubBandGeometry {
                width: 1,
                height: 1,
            }],
            layer: 0,
        };
        let err = walk_packet_headers(
            &all,
            &[(0usize, geom, SegmentSplit::Single)],
            SopEphMode::None,
        )
        .unwrap_err();
        assert_eq!(err, Error::PacketHeaderOverrun);
    }

    #[test]
    fn walk_handles_three_sub_band_packet() {
        // Resolution > 0 packet with 3 sub-bands. Each sub-band has a
        // 1×1 code-block grid. We mark only the LH sub-band's block
        // as included.
        // Order per §B.10.8: zero-bit, then sub-band 0 (HL: 1 codeblock),
        // sub-band 1 (LH), sub-band 2 (HH).
        // Bits:
        //   1       — non-zero packet
        //   0       — HL block not included (tag-tree partial)
        //   1       — LH block included (tag-tree commit at threshold 1)
        //   1       — LH zero-bitplane value = 0
        //   0       — LH coding passes = 1
        //   0       — LH no Lblock increment
        //   100     — LH length 4
        //   0       — HH block not included
        let bits = vec![1u8, 0, 1, 1, 0, 0, 1, 0, 0, 0];
        let mut packed = pack_bits(&bits);
        packed.extend([0u8; 4]); // LH body bytes
        let geom = PacketGeometry {
            sub_bands: vec![
                SubBandGeometry {
                    width: 1,
                    height: 1,
                },
                SubBandGeometry {
                    width: 1,
                    height: 1,
                },
                SubBandGeometry {
                    width: 1,
                    height: 1,
                },
            ],
            layer: 0,
        };
        let mut state = PrecinctState::new();
        let h = decode_packet_header(
            &packed,
            &geom,
            &mut state,
            SopEphMode::None,
            SegmentSplit::Single,
        )
        .unwrap();
        assert!(h.non_zero_length);
        assert_eq!(h.contributions.len(), 3);
        assert!(!h.contributions[0].included);
        assert!(h.contributions[1].included);
        assert!(!h.contributions[2].included);
        assert_eq!(h.contributions[1].segment_lengths, vec![4]);
    }

    #[test]
    fn sop_marker_consumed_when_enabled() {
        // SOP marker before an empty-packet header. SOP segment is:
        // 0xFF91 (marker) + 0x0004 (Lsop) + 0x0000 (Nsop) = 6 bytes.
        let mut packed = vec![0xFFu8, 0x91, 0x00, 0x04, 0x00, 0x00];
        packed.extend(&pack_bits(&[0])); // empty packet header
        let geom = PacketGeometry {
            sub_bands: vec![SubBandGeometry {
                width: 1,
                height: 1,
            }],
            layer: 0,
        };
        let mut state = PrecinctState::new();
        let h = decode_packet_header(
            &packed,
            &geom,
            &mut state,
            SopEphMode::SopOnly,
            SegmentSplit::Single,
        )
        .unwrap();
        assert!(!h.non_zero_length);
        assert_eq!(h.bytes_consumed, packed.len());
    }

    #[test]
    fn peek_sop_nsop_reads_sequence_number() {
        // SOP with Nsop = 0x0123.
        let bytes = [0xFFu8, 0x91, 0x00, 0x04, 0x01, 0x23, 0xAA];
        assert_eq!(peek_sop_nsop(&bytes).unwrap(), Some(0x0123));
        // No SOP marker → None (the slice is left for the header reader).
        let no_sop = pack_bits(&[1, 0, 1]);
        assert_eq!(peek_sop_nsop(&no_sop).unwrap(), None);
        // SOP marker code but truncated body → rejected.
        assert!(peek_sop_nsop(&[0xFFu8, 0x91, 0x00]).is_err());
        // Wrong Lsop → rejected.
        assert!(peek_sop_nsop(&[0xFFu8, 0x91, 0x00, 0x06, 0x00, 0x00]).is_err());
    }

    /// Build the two-packet body of `walk_two_packets_same_precinct_*`
    /// with an SOP marker (carrying `nsop0` / `nsop1`) prefixing each
    /// packet, plus the matching geometry pair.
    fn two_sop_packets(
        nsop0: u16,
        nsop1: u16,
    ) -> (Vec<u8>, [(usize, PacketGeometry, SegmentSplit); 2]) {
        let mut all = Vec::new();
        all.extend([0xFFu8, 0x91, 0x00, 0x04]);
        all.extend(nsop0.to_be_bytes());
        all.extend(pack_bits(&[1, 1, 1, 0, 0, 1, 0, 1]));
        all.extend([0u8; 5]); // 5 body bytes
        all.extend([0xFFu8, 0x91, 0x00, 0x04]);
        all.extend(nsop1.to_be_bytes());
        all.extend(pack_bits(&[1, 1, 0, 0, 0, 1, 0]));
        all.extend([0u8; 2]); // 2 body bytes
        let g = |layer| PacketGeometry {
            sub_bands: vec![SubBandGeometry {
                width: 1,
                height: 1,
            }],
            layer,
        };
        (
            all,
            [
                (0usize, g(0), SegmentSplit::Single),
                (0usize, g(1), SegmentSplit::Single),
            ],
        )
    }

    #[test]
    fn walk_validates_sequential_sop_nsop() {
        // §A.8.1: Nsop = 0, 1 for the first two packets of a coded tile.
        let (body, packets) = two_sop_packets(0, 1);
        let headers = walk_packet_headers(&body, &packets, SopEphMode::SopOnly).unwrap();
        assert_eq!(headers.len(), 2);
        assert!(headers[0].non_zero_length);
        assert!(headers[1].non_zero_length);
    }

    #[test]
    fn walk_rejects_out_of_order_sop_nsop() {
        // Second packet claims Nsop = 5 where the tile ordinal is 1 — a
        // desynchronised / lost-packet signature, rejected.
        let (body, packets) = two_sop_packets(0, 5);
        assert!(walk_packet_headers(&body, &packets, SopEphMode::SopOnly).is_err());
    }

    #[test]
    fn walk_accepts_out_of_order_nsop_when_packet_omits_sop() {
        // §A.8.1: the SOP marker is optional per-packet even when allowed.
        // The first packet carries a (correct) SOP; the second omits it.
        // No Nsop is present on packet 1, so nothing is validated there —
        // the absent-SOP case must not be treated as a sequence error.
        let mut all = Vec::new();
        all.extend([0xFFu8, 0x91, 0x00, 0x04, 0x00, 0x00]); // SOP, Nsop = 0
        all.extend(pack_bits(&[1, 1, 1, 0, 0, 1, 0, 1]));
        all.extend([0u8; 5]); // 5 body bytes
                              // Packet 1: no SOP marker.
        all.extend(pack_bits(&[1, 1, 0, 0, 0, 1, 0]));
        all.extend([0u8; 2]); // 2 body bytes
        let g = |layer| PacketGeometry {
            sub_bands: vec![SubBandGeometry {
                width: 1,
                height: 1,
            }],
            layer,
        };
        let headers = walk_packet_headers(
            &all,
            &[
                (0usize, g(0), SegmentSplit::Single),
                (0usize, g(1), SegmentSplit::Single),
            ],
            SopEphMode::SopOnly,
        )
        .unwrap();
        assert_eq!(headers.len(), 2);
    }

    #[test]
    fn eph_marker_consumed_when_enabled() {
        // Empty packet header followed by EPH marker (2 bytes).
        let mut packed = pack_bits(&[0]);
        packed.extend([0xFFu8, 0x92]); // EPH
        let geom = PacketGeometry {
            sub_bands: vec![SubBandGeometry {
                width: 1,
                height: 1,
            }],
            layer: 0,
        };
        let mut state = PrecinctState::new();
        let h = decode_packet_header(
            &packed,
            &geom,
            &mut state,
            SopEphMode::EphOnly,
            SegmentSplit::Single,
        )
        .unwrap();
        assert!(!h.non_zero_length);
        assert_eq!(h.bytes_consumed, packed.len());
    }

    #[test]
    fn precinct_state_layout_mismatch_rejected() {
        // Reuse a precinct's state with a different sub-band layout.
        let geom1 = PacketGeometry {
            sub_bands: vec![SubBandGeometry {
                width: 1,
                height: 1,
            }],
            layer: 0,
        };
        let geom2 = PacketGeometry {
            sub_bands: vec![SubBandGeometry {
                width: 2,
                height: 2,
            }],
            layer: 1,
        };
        let mut state = PrecinctState::new();
        // First call seeds the layout.
        let bits = pack_bits(&[0]);
        decode_packet_header(
            &bits,
            &geom1,
            &mut state,
            SopEphMode::None,
            SegmentSplit::Single,
        )
        .unwrap();
        // Second call with mismatching geometry must be rejected.
        let err = decode_packet_header(
            &bits,
            &geom2,
            &mut state,
            SopEphMode::None,
            SegmentSplit::Single,
        )
        .unwrap_err();
        assert_eq!(err, Error::InvalidPacketHeader);
    }

    // -- §B.10 packet-header encoder round-trip ------------------------

    #[test]
    fn bit_writer_round_trips_through_bit_reader() {
        // Random bits through the stuffing writer then the stuffing
        // reader must be identity — including forced 0xFF runs.
        let mut state = 0x1234_5678u32;
        // A long run of 1s early to force 0xFF bytes + stuff bits.
        let mut bits = vec![1u8; 64];
        for _ in 0..500 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            bits.push(((state >> 17) & 1) as u8);
        }
        let mut w = PacketBitWriter::new();
        for &b in &bits {
            w.write_bit(b);
        }
        let bytes = w.finish();
        let mut r = PacketBitReader::new(&bytes);
        for (i, &b) in bits.iter().enumerate() {
            assert_eq!(r.read_bit().unwrap(), b, "bit {i}");
        }
    }

    #[test]
    fn coding_passes_codeword_round_trips() {
        for passes in 1..=164u32 {
            let mut w = PacketBitWriter::new();
            encode_coding_passes(passes, &mut w);
            let bytes = w.finish();
            let mut r = PacketBitReader::new(&bytes);
            assert_eq!(decode_coding_passes(&mut r).unwrap(), passes);
        }
    }

    #[test]
    fn segment_length_round_trips_with_lblock_growth() {
        // A sequence of (passes, length) pairs across one code-block:
        // encoder and decoder Lblock must stay in lock-step, including
        // increases and the floor(log2 passes) widening.
        let seq: &[(u32, u32)] = &[
            (1, 5),      // fits in initial Lblock=3
            (1, 200),    // needs 8 bits → k=5 increase
            (3, 900),    // widened by floor(log2 3)=1
            (7, 65_000), // deep growth
            (2, 1),      // small after growth (no shrink)
        ];
        let mut enc_state = LblockState::default();
        let mut w = PacketBitWriter::new();
        for &(p, len) in seq {
            encode_segment_length(&mut enc_state, p, len, &mut w);
        }
        let bytes = w.finish();
        let mut dec_state = LblockState::default();
        let mut r = PacketBitReader::new(&bytes);
        for &(p, len) in seq {
            assert_eq!(
                decode_segment_length(&mut dec_state, p, &mut r).unwrap(),
                len
            );
        }
        assert_eq!(enc_state, dec_state);
    }

    #[test]
    fn tag_tree_encoder_round_trips_values() {
        // Full-value commits (the §B.10.5 zero-bitplane read) across a
        // 3×2 grid: the decoder must recover every leaf exactly, in the
        // same interleaved order.
        let leaves = [3u32, 0, 5, 2, 7, 1];
        let mut enc = TagTreeEncoder::new(3, 2, &leaves);
        let mut w = PacketBitWriter::new();
        for y in 0..2 {
            for x in 0..3 {
                enc.encode_value(x, y, &mut w);
            }
        }
        let bytes = w.finish();
        let mut dec = TagTree::new(3, 2);
        let mut r = PacketBitReader::new(&bytes);
        for y in 0..2 {
            for x in 0..3 {
                assert_eq!(
                    dec.decode_value(x, y, &mut r).unwrap(),
                    leaves[(y * 3 + x) as usize],
                    "leaf ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn tag_tree_encoder_round_trips_thresholds() {
        // Layered threshold queries (the §B.10.4 inclusion pattern):
        // interleave sub-threshold queries across layers and leaves, and
        // assert the decoder's answers match the ground truth.
        let leaves = [0u32, 2, 1, 3];
        let mut enc = TagTreeEncoder::new(2, 2, &leaves);
        let mut w = PacketBitWriter::new();
        let mut expect = Vec::new();
        for threshold in 1..=4u32 {
            for y in 0..2 {
                for x in 0..2 {
                    enc.encode_below_threshold(x, y, threshold, &mut w);
                    expect.push(leaves[(y * 2 + x) as usize] < threshold);
                }
            }
        }
        let bytes = w.finish();
        let mut dec = TagTree::new(2, 2);
        let mut r = PacketBitReader::new(&bytes);
        let mut i = 0;
        for threshold in 1..=4u32 {
            for y in 0..2 {
                for x in 0..2 {
                    assert_eq!(
                        dec.decode_below_threshold(x, y, threshold, &mut r).unwrap(),
                        expect[i],
                        "query {i} (x={x} y={y} t={threshold})"
                    );
                    i += 1;
                }
            }
        }
    }

    #[test]
    fn packet_header_encoder_round_trips_single_layer() {
        // One packet, one sub-band, 2×2 code-blocks, all included in
        // layer 0 with distinct P / passes / lengths.
        let geom = SubBandGeometry {
            width: 2,
            height: 2,
        };
        let first_layer = vec![0u32; 4];
        let zbp = vec![2u32, 0, 3, 1];
        let plans: Vec<CodeBlockPlan> = (0..4)
            .map(|i| CodeBlockPlan {
                included: true,
                zero_bit_planes: zbp[i],
                coding_passes: (i as u32 % 3) + 1,
                segments: vec![((i as u32 % 3) + 1, 10 + 40 * i as u32)],
            })
            .collect();
        let mut enc_state = PrecinctEncoderState::new(&[(geom, first_layer, zbp.clone())]);
        let header_bytes = encode_packet_header(&mut enc_state, 0, &plans);

        let geometry = PacketGeometry {
            sub_bands: vec![geom],
            layer: 0,
        };
        let mut dec_state = PrecinctState::new();
        let hdr = decode_packet_header(
            &header_bytes,
            &geometry,
            &mut dec_state,
            SopEphMode::None,
            SegmentSplit::Single,
        )
        .unwrap();
        assert!(hdr.non_zero_length);
        assert_eq!(hdr.contributions.len(), 4);
        for (i, c) in hdr.contributions.iter().enumerate() {
            assert!(c.included, "block {i}");
            assert_eq!(c.zero_bit_planes, Some(zbp[i]), "P block {i}");
            assert_eq!(c.coding_passes, plans[i].coding_passes, "passes {i}");
            assert_eq!(c.segment_lengths, vec![plans[i].segments[0].1]);
        }
        assert_eq!(hdr.bytes_consumed, header_bytes.len());
    }

    #[test]
    fn packet_header_encoder_round_trips_multi_layer_staggered() {
        // Three layers over a 2×1 + 2×2 (two sub-band) precinct with
        // staggered first-inclusion layers and per-layer inclusion gaps —
        // the decoder must track inclusion trees, P-on-first-inclusion,
        // Lblock growth, and the already-included one-bit path.
        let g0 = SubBandGeometry {
            width: 2,
            height: 1,
        };
        let g1 = SubBandGeometry {
            width: 2,
            height: 2,
        };
        // First inclusion layers per block (raster, sub-band order).
        let fl0 = vec![0u32, 1];
        let fl1 = vec![1u32, 0, 2, 3]; // block 3 of band 1 never included (< 3 layers)
        let zbp0 = vec![1u32, 0];
        let zbp1 = vec![0u32, 2, 1, 0];
        let mut enc_state = PrecinctEncoderState::new(&[
            (g0, fl0.clone(), zbp0.clone()),
            (g1, fl1.clone(), zbp1.clone()),
        ]);

        // Per-layer plans: a block is included from its first layer on,
        // except band-1 block (0,1) [index 2] skips layer 3's range (we
        // only run 3 layers so first_layer 3 = never included).
        let all_fl = [fl0.clone(), fl1.clone()].concat();
        let all_zbp = [zbp0.clone(), zbp1.clone()].concat();
        let mut headers = Vec::new();
        // (included, first-inclusion P, passes, segment length).
        type BlockTruth = (bool, Option<u32>, u32, u32);
        let mut truth: Vec<Vec<BlockTruth>> = Vec::new();
        for layer in 0..3u16 {
            let mut plans = Vec::new();
            let mut layer_truth = Vec::new();
            for i in 0..6usize {
                let included = all_fl[i] <= layer as u32;
                let first_time = all_fl[i] == layer as u32;
                let passes = if included {
                    1 + (i as u32 + layer as u32) % 4
                } else {
                    0
                };
                let seg = if included {
                    5 + 13 * i as u32 + 100 * layer as u32
                } else {
                    0
                };
                plans.push(CodeBlockPlan {
                    included,
                    zero_bit_planes: all_zbp[i],
                    coding_passes: passes,
                    segments: if included {
                        vec![(passes, seg)]
                    } else {
                        Vec::new()
                    },
                });
                layer_truth.push((
                    included,
                    if first_time { Some(all_zbp[i]) } else { None },
                    passes,
                    seg,
                ));
            }
            headers.push(encode_packet_header(&mut enc_state, layer, &plans));
            truth.push(layer_truth);
        }

        // Decode all three layers against one running precinct state.
        let mut dec_state = PrecinctState::new();
        for layer in 0..3usize {
            let geometry = PacketGeometry {
                sub_bands: vec![g0, g1],
                layer: layer as u16,
            };
            let hdr = decode_packet_header(
                &headers[layer],
                &geometry,
                &mut dec_state,
                SopEphMode::None,
                SegmentSplit::Single,
            )
            .unwrap();
            assert_eq!(hdr.contributions.len(), 6, "layer {layer}");
            for (i, c) in hdr.contributions.iter().enumerate() {
                let (included, p, passes, seg) = truth[layer][i];
                assert_eq!(c.included, included, "layer {layer} block {i}");
                assert_eq!(c.zero_bit_planes, p, "layer {layer} block {i} P");
                assert_eq!(c.coding_passes, passes, "layer {layer} block {i}");
                if included {
                    assert_eq!(c.segment_lengths, vec![seg], "layer {layer} block {i}");
                } else {
                    assert!(c.segment_lengths.is_empty());
                }
            }
            assert_eq!(hdr.bytes_consumed, headers[layer].len(), "layer {layer}");
        }
    }

    #[test]
    fn packet_header_encoder_empty_packet() {
        // An all-excluded plan list emits the §B.10.3 one-byte empty
        // packet header, and the decoder reads it back as empty.
        let geom = SubBandGeometry {
            width: 1,
            height: 1,
        };
        let mut enc_state = PrecinctEncoderState::new(&[(geom, vec![5u32], vec![0u32])]);
        let plans = vec![CodeBlockPlan {
            included: false,
            zero_bit_planes: 0,
            coding_passes: 0,
            segments: Vec::new(),
        }];
        let bytes = encode_packet_header(&mut enc_state, 0, &plans);
        assert_eq!(bytes.len(), 1);
        let geometry = PacketGeometry {
            sub_bands: vec![geom],
            layer: 0,
        };
        let mut dec_state = PrecinctState::new();
        let hdr = decode_packet_header(
            &bytes,
            &geometry,
            &mut dec_state,
            SopEphMode::None,
            SegmentSplit::Single,
        )
        .unwrap();
        assert!(!hdr.non_zero_length);
        assert!(hdr.contributions.is_empty());
    }
}
