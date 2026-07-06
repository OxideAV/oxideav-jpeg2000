//! High-Throughput JPEG 2000 (HTJ2K) **forward** block coder — the
//! encode-side dual of [`crate::ht`] (ITU-T T.814 | ISO/IEC
//! 15444-15:2019).
//!
//! T.814 normatively specifies only the *decoding* procedures; this
//! module constructs the exact bit-streams those procedures consume:
//!
//! * a **MagSgn writer** — the dual of the §7.1.2 `importMagSgnBit`
//!   state machine (little-endian bit packing, forward byte order, a
//!   7-bit byte with a zero stuffing MSB after any `0xFF`),
//! * a **MEL writer + adaptive run-length symbol encoder** — the dual
//!   of §7.1.3 `importMELBit` / §7.3.3 `decodeMELSym` (big-endian bit
//!   packing, the Table 2 `MEL_E` exponent state machine),
//! * a **VLC writer** — the dual of §7.1.4 `importVLCBit` (bits packed
//!   little-endian into bytes laid out *backward* from the segment
//!   tail, the greater-than-`0x8F` / low-seven-ones stuffing rule, and
//!   the 4-bit initial word sharing the second-last byte with `Scup`),
//! * the **§7.3 HT cleanup pass** run forward: quad significance
//!   patterns, §7.3.5 CxtVLC codeword selection out of the Annex C
//!   tables, §7.3.6 U-VLC residual coding with the §7.3.4 quad-pair
//!   interleave and first-line-pair MEL gates, the §7.3.7 exponent
//!   predictor, and §7.3.8 MagSgn value emission.
//!
//! [`encode_ht_cleanup_segment`] assembles the three streams into one
//! HT cleanup segment (`MagSgn ‖ MEL ‖ VLC`, `Scup` in the final
//! twelve bits per §7.1.1) that [`crate::ht::decode_ht_codeblock`]
//! decodes back bit-exactly — the round-trip against the crate's own
//! independently written decoder is the validation harness.
//!
//! ## Encoder-choice conventions
//!
//! Where the decoder admits several encodings, this encoder picks:
//!
//! * `U_q = max(κ_q, max E_n over significant samples)` and
//!   `u_q = U_q − κ_q` (so `u_off = 1` exactly when some significant
//!   exponent exceeds the predictor),
//! * EMB patterns `ε̄ᵏ_q` = the significant samples with
//!   `E_n = U_q` when `u_off = 1` (else all-zero), `ε̄¹_q` = the
//!   `ε̄ᵏ_q` bits (their §7.3.8 known bit — the `U_q − 1` magnitude
//!   bit — is 1 whenever `E_n = U_q ≥ 2`, which `u_off = 1` implies
//!   since `κ_q ≥ 1`),
//! * partial trailing bytes pad with `1` bits, and a pending MEL run
//!   flushes as a single `1` (a claimed-complete run the decoder never
//!   reads past).
//!
//! All truth in this module comes from `docs/image/jpeg2000/` (T.814 |
//! 15444-15). No external HTJ2K implementation was consulted.

use crate::ht::{
    apply_sig_pattern, exponent_predictor, magnitude_exponent, quad_context, HtGrid, MEL_E,
};
use crate::ht_tables::{CxtVlcEntry, CXT_VLC_TABLE_0, CXT_VLC_TABLE_1};
use crate::Error;

// ---------------------------------------------------------------------------
// §7.1.2 dual — MagSgn bit-stream writer (forward, little-endian).
// ---------------------------------------------------------------------------

/// Writes the MagSgn bit-stream: bits pack little-endian into forward
/// bytes; a byte following an emitted `0xFF` carries only 7 payload
/// bits (its MSB is the zero stuffing bit `importMagSgnBit` checks).
struct MagSgnWriter {
    out: Vec<u8>,
    tmp: u32,
    used: u32,
    cap: u32,
}

impl MagSgnWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            tmp: 0,
            used: 0,
            cap: 8,
        }
    }

    fn bit(&mut self, b: u32) {
        self.tmp |= (b & 1) << self.used;
        self.used += 1;
        if self.used == self.cap {
            self.out.push(self.tmp as u8);
            self.cap = if self.tmp == 0xFF { 7 } else { 8 };
            self.tmp = 0;
            self.used = 0;
        }
    }

    /// Emit `m` bits of `val`, LSB first (§7.3.8 `decodeMagSgnValue`
    /// inner loop, reversed).
    fn bits(&mut self, val: u32, m: u32) {
        for i in 0..m {
            self.bit((val >> i) & 1);
        }
    }

    /// Pad any partial byte with `1` bits and return the stream. The
    /// reader never requests the pad (it reads exactly the signalled
    /// `m_n` bits per sample), and a capacity-7 byte pads to at most
    /// `0x7F`, keeping its stuffing MSB zero.
    fn finish(mut self) -> Vec<u8> {
        if self.used > 0 {
            for i in self.used..self.cap {
                self.tmp |= 1 << i;
            }
            self.out.push(self.tmp as u8);
        }
        self.out
    }
}

// ---------------------------------------------------------------------------
// §7.1.3 dual — MEL bit-stream writer (forward, big-endian) + §7.3.3
// dual — adaptive run-length symbol encoder.
// ---------------------------------------------------------------------------

/// Writes the MEL bit-stream: bits pack big-endian (MSB first) into
/// forward bytes; the byte after an emitted `0xFF` carries 7 payload
/// bits in its low positions (`importMELBit` starts that byte at bit 6).
struct MelWriter {
    out: Vec<u8>,
    tmp: u32,
    used: u32,
    cap: u32,
}

impl MelWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            tmp: 0,
            used: 0,
            cap: 8,
        }
    }

    fn bit(&mut self, b: u32) {
        self.tmp = (self.tmp << 1) | (b & 1);
        self.used += 1;
        if self.used == self.cap {
            // A capacity-7 byte occupies bits 6..0; bit 7 stays 0.
            self.out.push(self.tmp as u8);
            self.cap = if self.tmp == 0xFF { 7 } else { 8 };
            self.tmp = 0;
            self.used = 0;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.used > 0 {
            // Left-align the partial byte and pad the tail with 1s (the
            // reader consumes MSB-first and never requests the pad).
            let pad = self.cap - self.used;
            self.tmp = (self.tmp << pad) | ((1 << pad) - 1);
            self.out.push(self.tmp as u8);
        }
        self.out
    }
}

/// §7.3.3 dual — the adaptive MEL run-length **symbol** encoder: runs
/// of `0` symbols compress through the Table 2 exponent ladder, each
/// complete `2^MEL_E[k]` run emitting a `1` bit (ladder up), and each
/// terminating `1` symbol emitting a `0` bit plus the partial run
/// length in `MEL_E[k]` big-endian bits (ladder down).
struct MelEncoder {
    writer: MelWriter,
    k: usize,
    run: u32,
}

impl MelEncoder {
    fn new() -> Self {
        Self {
            writer: MelWriter::new(),
            k: 0,
            run: 0,
        }
    }

    fn sym(&mut self, s: u8) {
        if s == 0 {
            self.run += 1;
            if self.run == 1 << MEL_E[self.k] {
                self.writer.bit(1);
                self.run = 0;
                self.k = (self.k + 1).min(12);
            }
        } else {
            let eval = MEL_E[self.k] as u32;
            self.writer.bit(0);
            for i in (0..eval).rev() {
                self.writer.bit((self.run >> i) & 1);
            }
            self.run = 0;
            self.k = self.k.saturating_sub(1);
        }
    }

    /// Close a pending zero-run by claiming a complete run: the decoder
    /// gets at least as many zeros as remain owed and never asks for
    /// more.
    fn finish(mut self) -> Vec<u8> {
        if self.run > 0 {
            self.writer.bit(1);
        }
        self.writer.finish()
    }
}

// ---------------------------------------------------------------------------
// §7.1.4 dual — VLC bit-stream writer (reverse byte order).
// ---------------------------------------------------------------------------

/// Collects the VLC bit sequence in decode order, then packs it into
/// the backward-growing byte layout `importVLCBit` unwinds: the first
/// four (or three) bits fill the high nibble of the second-last
/// segment byte, subsequent bytes fill from position `Lcup − 3`
/// downward, and a byte whose predecessor-in-read-order exceeds `0x8F`
/// while its own low seven bits are all ones carries only 7 payload
/// bits (bit 7 is the zero stuffing bit).
struct VlcWriter {
    bits: Vec<u8>,
}

impl VlcWriter {
    fn new() -> Self {
        Self { bits: Vec::new() }
    }

    fn bit(&mut self, b: u32) {
        self.bits.push((b & 1) as u8);
    }

    /// Emit codeword `cwd` of `len` bits, LSB first (§7.3.5 import
    /// order).
    fn codeword(&mut self, cwd: u32, len: u32) {
        for i in 0..len {
            self.bit((cwd >> i) & 1);
        }
    }

    /// Pack into `(initial_nibble, body)`: the nibble lands in the
    /// high half of byte `Lcup − 2`; `body[i]` is the byte at position
    /// `Lcup − 3 − i` (read order — the caller reverses it for
    /// placement). Exhausted positions pad with `1` bits.
    fn finish(self) -> (u8, Vec<u8>) {
        let mut it = self.bits.into_iter();
        // `real` counts data bits placed in the current word so a
        // trailing word holding only pad is dropped — but one whose
        // data bits happen to be all ones is NOT (that exact confusion
        // once dropped a final all-ones codeword byte).
        let mut real_total = 0usize;
        let total = it.len();
        let mut next = |real: &mut u32| -> u32 {
            match it.next() {
                Some(b) => {
                    *real += 1;
                    u32::from(b)
                }
                None => 1, // pad with 1 bits
            }
        };
        // Initial word: 3 bits; a 4th only when the low three are not
        // all ones (`initVLC` sets `vlc_bits = 3` when they are — the
        // nibble's top bit is then the stuffing position, left 0).
        let mut nreal = 0u32;
        let mut nibble: u32 = 0;
        for i in 0..3 {
            nibble |= next(&mut nreal) << i;
        }
        if nibble & 7 < 7 {
            nibble |= next(&mut nreal) << 3;
        }
        real_total += nreal as usize;
        // `vlc_last` as the reader sees it: the modDcup view of byte
        // Lcup − 2 (low nibble forced to 1s).
        let mut vlc_last: u32 = (nibble << 4) | 0x0F;
        let mut body: Vec<u8> = Vec::new();
        while real_total < total {
            let mut real = 0u32;
            let mut cur: u32 = 0;
            for i in 0..7 {
                cur |= next(&mut real) << i;
            }
            // Stuffing rule: after a byte > 0x8F, a low-seven-ones byte
            // delivers only 7 bits (bit 7 stays 0). Otherwise bit 7
            // carries the 8th payload bit.
            if !(vlc_last > 0x8F && (cur & 0x7F) == 0x7F) {
                cur |= next(&mut real) << 7;
            }
            body.push(cur as u8);
            vlc_last = cur;
            real_total += real as usize;
        }
        (nibble as u8, body)
    }
}

// ---------------------------------------------------------------------------
// §7.3.6 dual — U-VLC residual encoding.
// ---------------------------------------------------------------------------

/// Split `u ≥ 1` into the Formula (3) `(prefix, suffix, extension)`
/// triple: `u = pfx + sfx + 4·ext`.
fn u_parts(u: u32) -> (u32, u32, u32) {
    match u {
        1 => (1, 0, 0),
        2 => (2, 0, 0),
        3 | 4 => (3, u - 3, 0),
        _ => {
            let rem = u - 5;
            if rem < 28 {
                (5, rem, 0)
            } else {
                let sfx = 28 + (rem - 28) % 4;
                (5, sfx, (rem - sfx) / 4)
            }
        }
    }
}

/// §7.3.6 `decodeUPrefix` dual: "1" → 1, "01" → 2, "001" → 3, "000" → 5.
fn encode_u_prefix(vlc: &mut VlcWriter, pfx: u32) {
    match pfx {
        1 => vlc.bit(1),
        2 => {
            vlc.bit(0);
            vlc.bit(1);
        }
        3 => {
            vlc.bit(0);
            vlc.bit(0);
            vlc.bit(1);
        }
        _ => {
            vlc.bit(0);
            vlc.bit(0);
            vlc.bit(0);
        }
    }
}

/// §7.3.6 `decodeUSuffix` dual: none below prefix 3, one bit at
/// prefix 3, five little-endian bits at prefix 5.
fn encode_u_suffix(vlc: &mut VlcWriter, pfx: u32, sfx: u32) {
    if pfx < 3 {
        return;
    }
    if pfx == 3 {
        vlc.bit(sfx);
        return;
    }
    for i in 0..5 {
        vlc.bit((sfx >> i) & 1);
    }
}

/// §7.3.6 `decodeUExtension` dual: four little-endian bits when the
/// suffix reached 28.
fn encode_u_extension(vlc: &mut VlcWriter, sfx: u32, ext: u32) {
    if sfx < 28 {
        return;
    }
    for i in 0..4 {
        vlc.bit((ext >> i) & 1);
    }
}

// ---------------------------------------------------------------------------
// §7.3.5 dual — CxtVLC codeword selection.
// ---------------------------------------------------------------------------

/// Choose an Annex C entry for `(cq, ρ, u_off)` compatible with the
/// quad's magnitude data.
///
/// `b` carries, for each significant sample `j`, bit `U_q − 1` of the
/// §7.3.8 value `v_n = 2(μ_n − 1) + s_n` — the bit the decoder
/// reconstructs from `ε̄¹` whenever `ε̄ᵏ` marks the sample (it then
/// reads only `m_n = U_q − 1` MagSgn bits). An entry is usable iff
/// every sample its `ε̄ᵏ` marks has exactly the `ε̄¹` bit the table
/// promises (`ε̄¹ ∧ ε̄ᵏ = b ∧ ε̄ᵏ`); unmarked samples read all
/// `U_q` bits with a zero known bit, which always reconstructs
/// (`v_n < 2^{E_n} ≤ 2^{U_q}`). The Annex C tables form a covering
/// code over the `b` patterns that can arise (when `u_off = 1` the
/// exponent-bound samples force `b = 1` at their positions), so a
/// match always exists for conforming data. Among the candidates the
/// one marking the most samples (fewest MagSgn bits), then the
/// shortest codeword, wins.
fn choose_cxt_vlc(
    first_line_pair: bool,
    cq: u8,
    rho: u8,
    u_off: u8,
    b: u8,
) -> Result<&'static CxtVlcEntry, Error> {
    let table: &[CxtVlcEntry] = if first_line_pair {
        &CXT_VLC_TABLE_0
    } else {
        &CXT_VLC_TABLE_1
    };
    table
        .iter()
        .filter(|e| {
            e.cq == cq
                && e.rho == rho
                && e.u_off == u_off
                && e.e_k & !rho == 0
                && e.e_1 & !e.e_k == 0
                && (e.e_1 & e.e_k) == (b & e.e_k)
        })
        .max_by_key(|e| (e.e_k.count_ones(), std::cmp::Reverse(e.len)))
        .ok_or(Error::NotImplemented)
}

// ---------------------------------------------------------------------------
// §7.3 forward — one quad's derived coding quantities.
// ---------------------------------------------------------------------------

/// Everything the §7.3.4 interleave needs for one quad, computed
/// before any stream bytes are emitted for its pair.
struct QuadPlan {
    q: usize,
    cq: u8,
    rho: u8,
    u_off: u8,
    u_q: u32,
    u_big: u32,
    e_k: u8,
    e_1: u8,
    cwd: u8,
    len: u8,
}

/// Derive quad `q`'s significance pattern, context, `U_q` / `u_q` and
/// EMB patterns from the grid samples (which must already hold μ / s /
/// σ / E for *previous* quads; this quad's σ is applied here so the
/// next quad's context sees it, mirroring the decoder's order).
fn plan_quad(
    grid: &mut HtGrid,
    q: usize,
    mu: &[u32],
    sign: &[bool],
    first_line_pair: bool,
) -> Result<QuadPlan, Error> {
    let mut rho: u8 = 0;
    for j in 0..4 {
        if let Some((x, y)) = grid.coord(q, j) {
            if mu[x + y * grid.wblk] != 0 {
                rho |= 1 << j;
            }
        }
    }
    let cq = quad_context(grid, q);
    apply_sig_pattern(grid, q, rho);
    if rho == 0 {
        let (cwd, len) = if cq == 0 {
            // AZC quad answered entirely by the MEL symbol — no
            // codeword is emitted.
            (0, 0)
        } else {
            let e = choose_cxt_vlc(first_line_pair, cq, 0, 0, 0)?;
            (e.cwd, e.len)
        };
        return Ok(QuadPlan {
            q,
            cq,
            rho,
            u_off: 0,
            u_q: 0,
            u_big: 0,
            e_k: 0,
            e_1: 0,
            cwd,
            len,
        });
    }
    let kappa = exponent_predictor(grid, q, rho);
    let mut e_max = 0u32;
    for j in 0..4 {
        if rho & (1 << j) != 0 {
            if let Some((x, y)) = grid.coord(q, j) {
                e_max = e_max.max(magnitude_exponent(mu[x + y * grid.wblk]));
            }
        }
    }
    let u_big = kappa.max(e_max);
    let u_q = u_big - kappa;
    let u_off = u8::from(u_q != 0);
    // The §7.3.8 known-bit pattern the data dictates: bit `U_q − 1` of
    // each significant sample's value v_n = 2(μ_n − 1) + s_n.
    let mut b: u8 = 0;
    for j in 0..4 {
        if rho & (1 << j) != 0 {
            if let Some((x, y)) = grid.coord(q, j) {
                let idx = x + y * grid.wblk;
                let val = 2 * (mu[idx] - 1) + u32::from(sign[idx]);
                b |= (((val >> (u_big - 1)) & 1) as u8) << j;
            }
        }
    }
    let entry = choose_cxt_vlc(first_line_pair, cq, rho, u_off, b)?;
    Ok(QuadPlan {
        q,
        cq,
        rho,
        u_off,
        u_q,
        u_big,
        e_k: entry.e_k,
        e_1: entry.e_1,
        cwd: entry.cwd,
        len: entry.len,
    })
}

/// §7.3.8 forward — write quad `q`'s significant-sample MagSgn values
/// and commit μ / s / E into the grid (feeding later predictors).
fn emit_quad_magsgn(
    grid: &mut HtGrid,
    magsgn: &mut MagSgnWriter,
    plan: &QuadPlan,
    mu: &[u32],
    sign: &[bool],
) -> Result<(), Error> {
    let q = plan.q;
    grid.quads[q].rho = plan.rho;
    grid.quads[q].e_k = plan.e_k;
    grid.quads[q].e_1 = plan.e_1;
    grid.quads[q].u_q = plan.u_q;
    grid.quads[q].u_big = plan.u_big;
    for j in 0..4 {
        if plan.rho & (1 << j) == 0 {
            continue;
        }
        let (x, y) = grid.coord(q, j).ok_or(Error::NotImplemented)?;
        let idx = x + y * grid.wblk;
        let m = mu[idx];
        let s = u32::from(sign[idx]);
        // §7.3.8: v_n = 2(μ_n − 1) + s_n; the decoder reads
        // m_n = U_q − k_n bits and adds ī_n · 2^m_n.
        let val = 2 * (m - 1) + s;
        let k_n = u32::from((plan.e_k >> j) & 1);
        let i_n = u32::from((plan.e_1 >> j) & 1);
        let m_n = plan.u_big - k_n;
        // The encoded value must fit m_n bits below the known bit.
        if val >> m_n != i_n {
            return Err(Error::NotImplemented);
        }
        magsgn.bits(val, m_n);
        grid.mu[idx] = m;
        grid.sign[idx] = sign[idx];
        grid.exp[idx] = magnitude_exponent(m);
    }
    Ok(())
}

/// Encode one code-block's samples into an **HT cleanup segment**
/// (T.814 §7.1.1: `MagSgn ‖ MEL ‖ VLC` with `Scup` in the final twelve
/// bits) that [`crate::ht::decode_ht_codeblock`] reverses bit-exactly.
///
/// * `mu` — per-sample magnitudes (raster order, `width × height`),
///   the values the cleanup pass transports (`μ_n ≥ 1` significant,
///   `0` insignificant).
/// * `sign` — per-sample signs (`true` ≡ negative), meaningful where
///   `μ_n ≠ 0`.
///
/// Returns the segment bytes; a block whose streams would exceed the
/// §7.1.1 `Scup ≤ 4079` bound is rejected with
/// [`Error::NotImplemented`].
pub fn encode_ht_cleanup_segment(
    mu: &[u32],
    sign: &[bool],
    width: usize,
    height: usize,
) -> Result<Vec<u8>, Error> {
    assert!(width >= 1 && height >= 1);
    assert_eq!(mu.len(), width * height);
    assert_eq!(sign.len(), width * height);

    let mut grid = HtGrid::new(width, height);
    let mut magsgn = MagSgnWriter::new();
    let mut mel = MelEncoder::new();
    let mut vlc = VlcWriter::new();

    let qw = grid.qw;
    let qh = grid.qh;

    for qy in 0..qh {
        let first_line_pair = qy == 0;
        let row_start = qy * qw;
        let mut qx = 0;
        while qx < qw {
            let q1 = row_start + qx;
            let has_q2 = qx + 1 < qw;

            // --- Significance + EMB (encodeSigEMB duals) ---
            let p1 = plan_quad(&mut grid, q1, mu, sign, first_line_pair)?;
            emit_sig_emb(&mut mel, &mut vlc, &p1);
            let p2 = if has_q2 {
                let p = plan_quad(&mut grid, q1 + 1, mu, sign, first_line_pair)?;
                emit_sig_emb(&mut mel, &mut vlc, &p);
                Some(p)
            } else {
                None
            };

            // --- U-VLC (§7.3.4 / §7.3.6 interleave duals) ---
            emit_u_pair(&mut mel, &mut vlc, first_line_pair, &p1, p2.as_ref())?;

            // --- MagSgn (§7.3.7 / §7.3.8) ---
            emit_quad_magsgn(&mut grid, &mut magsgn, &p1, mu, sign)?;
            if let Some(p) = &p2 {
                emit_quad_magsgn(&mut grid, &mut magsgn, p, mu, sign)?;
            }

            qx += 2;
        }
    }

    // -- Assemble the segment (§7.1.1) ----------------------------------
    let magsgn_bytes = magsgn.finish();
    let mel_bytes = mel.finish();
    let (vlc_nibble, vlc_body) = vlc.finish();

    // Scup counts the MEL + VLC suffix, including the two bytes whose
    // final twelve bits carry Scup itself.
    let scup = mel_bytes.len() + vlc_body.len() + 2;
    if !(2..=4079).contains(&scup) {
        return Err(Error::NotImplemented);
    }
    let mut seg = magsgn_bytes;
    seg.extend_from_slice(&mel_bytes);
    seg.extend(vlc_body.iter().rev());
    seg.push((vlc_nibble << 4) | ((scup & 0x0F) as u8));
    seg.push((scup >> 4) as u8);
    Ok(seg)
}

/// §7.3.5 `decodeSigEMB` dual: an all-zero-context quad first spends a
/// MEL symbol (0 = entirely insignificant, no codeword); every other
/// case emits the Annex C codeword for the quad's tuple.
fn emit_sig_emb(mel: &mut MelEncoder, vlc: &mut VlcWriter, plan: &QuadPlan) {
    if plan.cq == 0 {
        mel.sym(u8::from(plan.rho != 0));
        if plan.rho == 0 {
            return;
        }
    }
    vlc.codeword(u32::from(plan.cwd), u32::from(plan.len));
}

/// §7.3.4 / §7.3.6 dual of `decode_u_pair`: the interleaved residual
/// coding for a quad-pair, honouring the first-line-pair MEL gate (a
/// `1` symbol switches both quads to Formula (4) `u − 2` coding; a `0`
/// symbol keeps Formula (3), with the second quad collapsing to one
/// raw bit when the first residual exceeds 2).
fn emit_u_pair(
    mel: &mut MelEncoder,
    vlc: &mut VlcWriter,
    first_line_pair: bool,
    p1: &QuadPlan,
    p2: Option<&QuadPlan>,
) -> Result<(), Error> {
    let off1 = p1.u_off == 1;
    let off2 = p2.is_some_and(|p| p.u_off == 1);
    let u1 = p1.u_q;
    let u2 = p2.map_or(0, |p| p.u_q);

    if first_line_pair && off1 && off2 {
        if u1 >= 3 && u2 >= 3 {
            mel.sym(1);
            let (pa, sa, ea) = u_parts(u1 - 2);
            let (pb, sb, eb) = u_parts(u2 - 2);
            encode_u_prefix(vlc, pa);
            encode_u_prefix(vlc, pb);
            encode_u_suffix(vlc, pa, sa);
            encode_u_suffix(vlc, pb, sb);
            encode_u_extension(vlc, sa, ea);
            encode_u_extension(vlc, sb, eb);
        } else {
            mel.sym(0);
            let (pa, sa, ea) = u_parts(u1);
            encode_u_prefix(vlc, pa);
            if u1 > 2 {
                // u2 ∈ {1, 2} here (u2 ≥ 3 took the branch above).
                // §7.3.4 / §7.3.6: the single bit replaces q2's
                // *prefix step*, so it precedes q1's suffix.
                debug_assert!((1..=2).contains(&u2));
                vlc.bit(u2 - 1);
                encode_u_suffix(vlc, pa, sa);
                encode_u_extension(vlc, sa, ea);
            } else {
                let (pb, sb, eb) = u_parts(u2);
                encode_u_prefix(vlc, pb);
                encode_u_suffix(vlc, pa, sa);
                encode_u_suffix(vlc, pb, sb);
                encode_u_extension(vlc, sa, ea);
                encode_u_extension(vlc, sb, eb);
            }
        }
        return Ok(());
    }

    // General case: prefix-first interleave across the pair.
    let a = off1.then(|| u_parts(u1));
    let b = off2.then(|| u_parts(u2));
    if let Some((pa, _, _)) = a {
        encode_u_prefix(vlc, pa);
    }
    if let Some((pb, _, _)) = b {
        encode_u_prefix(vlc, pb);
    }
    if let Some((pa, sa, _)) = a {
        encode_u_suffix(vlc, pa, sa);
    }
    if let Some((pb, sb, _)) = b {
        encode_u_suffix(vlc, pb, sb);
    }
    if let Some((_, sa, ea)) = a {
        encode_u_extension(vlc, sa, ea);
    }
    if let Some((_, sb, eb)) = b {
        encode_u_extension(vlc, sb, eb);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// §7.1.5 dual — SigProp bit-stream writer (forward, little-endian).
// ---------------------------------------------------------------------------

/// Writes the SigProp bit-stream: bits pack LSB-first into forward
/// bytes; the byte following an emitted `0xFF` carries only 7 payload
/// bits with a zero MSB (the §7.1.5 stuffing bit the reader validates).
struct SigPropWriter {
    out: Vec<u8>,
    tmp: u32,
    used: u32,
    cap: u32,
}

impl SigPropWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            tmp: 0,
            used: 0,
            cap: 8,
        }
    }

    fn bit(&mut self, b: u32) {
        self.tmp |= (b & 1) << self.used;
        self.used += 1;
        if self.used == self.cap {
            self.out.push(self.tmp as u8);
            self.cap = if self.tmp == 0xFF { 7 } else { 8 };
            self.tmp = 0;
            self.used = 0;
        }
    }

    /// Pad any partial byte with `0` bits (never read — the §7.4 pass
    /// consumes exactly the coded bits) and return the stream.
    fn finish(mut self) -> Vec<u8> {
        if self.used > 0 {
            self.out.push(self.tmp as u8);
        }
        self.out
    }
}

// ---------------------------------------------------------------------------
// §7.1.6 dual — MagRef bit-stream writer (backward byte layout).
// ---------------------------------------------------------------------------

/// Writes the MagRef bit-stream. The §7.1.6 reader walks the refinement
/// segment **backward** from its last byte, taking bits LSB-first and
/// dropping to 7 bits whenever the previously read byte exceeded `0x8F`
/// and the current byte's low seven bits are all ones (initial state
/// `0xFF`, so the very first byte read is subject to the rule). This
/// writer packs the bit sequence into bytes in *reader* order,
/// mirroring the same state machine; [`Self::finish`] returns the bytes
/// ready to be appended reversed at the segment tail.
struct MagRefWriter {
    /// Bytes in reader order (first-read byte first).
    out: Vec<u8>,
    tmp: u32,
    used: u32,
    prev: u32,
}

impl MagRefWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            tmp: 0,
            used: 0,
            prev: 0xFF,
        }
    }

    fn commit(&mut self) {
        self.out.push(self.tmp as u8);
        self.prev = self.tmp;
        self.tmp = 0;
        self.used = 0;
    }

    fn bit(&mut self, b: u32) {
        self.tmp |= (b & 1) << self.used;
        self.used += 1;
        // §7.1.6: with the previous byte above 0x8F, a byte whose low
        // seven bits are all ones carries only those 7 bits — close it
        // as 0x7F (zero MSB) so the reader's 7-bit rule fires exactly
        // where the writer stopped; otherwise a byte closes at 8 bits.
        let seven_ones = self.prev > 0x8F && (self.tmp & 0x7F) == 0x7F;
        if (self.used == 7 && seven_ones) || self.used == 8 {
            self.commit();
        }
    }

    /// Pad any partial byte with `0` bits (a zero-padded byte can never
    /// re-trigger the low-seven-ones rule retroactively; the pad is
    /// never read) and return the bytes in reader order.
    fn finish(mut self) -> Vec<u8> {
        if self.used > 0 {
            self.out.push(self.tmp as u8);
        }
        self.out
    }
}

/// §7.4 / §7.5 forward — build the HT refinement segment (one SigProp
/// pass plus one MagRef pass) for a code-block whose HT cleanup pass
/// transported `μ_n = mag_n >> 1` (i.e. the cleanup stopped one
/// bit-plane above the LSB and the refinement passes carry bit-plane
/// 0).
///
/// * `mag` — full per-sample magnitudes (raster order), whose bit 0 is
///   what the refinement passes code;
/// * `sign` — per-sample signs (`true` ≡ negative), emitted by the
///   §7.4 sign step for samples SigProp makes newly non-zero.
///
/// Mirrors the decoder's §7.4 stripe-oriented scan exactly: a sample
/// that is insignificant after cleanup (`mag >> 1 == 0`) receives a
/// SigProp bit iff its 8-neighbourhood significance / scan-causal
/// refinement OR (`mbr`) is non-zero; every cleanup-significant sample
/// receives a §7.5 MagRef bit. Returns `None` when some sample with
/// `mag == 1` is *not* reached by SigProp — such a block cannot decode
/// losslessly from this pass structure, so the caller must keep the
/// cleanup at bit-plane 0 instead.
pub fn encode_ht_refinement_segment(
    mag: &[u32],
    sign: &[bool],
    width: usize,
    height: usize,
) -> Option<Vec<u8>> {
    assert!(width >= 1 && height >= 1);
    assert_eq!(mag.len(), width * height);
    assert_eq!(sign.len(), width * height);
    let sigma = |idx: usize| mag[idx] >= 2;

    let mut sp = SigPropWriter::new();
    let mut mr = MagRefWriter::new();
    let mut r = vec![0u8; width * height];
    let mut z = vec![0u8; width * height];

    // §7.4 — stripes of 4 rows; within a stripe, 4-column groups run
    // all magnitude steps then all sign steps (same order the decoder
    // consumes them).
    let mut y0 = 0;
    while y0 < height {
        let rows = (height - y0).min(4);
        let mut x0 = 0;
        while x0 < width {
            let cols = (width - x0).min(4);
            for dx in 0..cols {
                for dy in 0..rows {
                    let x = x0 + dx;
                    let y = y0 + dy;
                    let idx = x + y * width;
                    if sigma(idx) {
                        continue;
                    }
                    let mut mbr = 0u8;
                    for (nx, ny) in crate::ht::neighbours8(x, y, width, height) {
                        let nidx = nx + ny * width;
                        mbr |= u8::from(sigma(nidx));
                        if crate::ht::scan_causal(nx, ny, x, y) {
                            mbr |= r[nidx];
                        }
                    }
                    if mbr != 0 {
                        z[idx] = 1;
                        r[idx] = (mag[idx] & 1) as u8;
                        sp.bit(mag[idx] & 1);
                    }
                }
            }
            for dx in 0..cols {
                for dy in 0..rows {
                    let x = x0 + dx;
                    let y = y0 + dy;
                    let idx = x + y * width;
                    if r[idx] != 0 {
                        // §7.4 sign step: 1 ≡ negative.
                        sp.bit(u32::from(sign[idx]));
                    }
                }
            }
            x0 += 4;
        }
        y0 += 4;
    }

    // Losslessness gate: every odd magnitude below the cleanup plane
    // must have been reached.
    for idx in 0..width * height {
        if mag[idx] == 1 && z[idx] == 0 {
            return None;
        }
    }

    // §7.5 — MagRef: one bit-plane-0 bit per cleanup-significant
    // sample, same stripe scan.
    let mut y0 = 0;
    while y0 < height {
        let rows = (height - y0).min(4);
        let mut x0 = 0;
        while x0 < width {
            let cols = (width - x0).min(4);
            for dx in 0..cols {
                for dy in 0..rows {
                    let idx = (x0 + dx) + (y0 + dy) * width;
                    if sigma(idx) {
                        mr.bit(mag[idx] & 1);
                    }
                }
            }
            x0 += 4;
        }
        y0 += 4;
    }

    // §7.1.5 / §7.1.6 — SigProp bytes extend forward from byte 0 of
    // the refinement segment; MagRef bytes extend backward from its
    // last byte.
    let mut seg = sp.finish();
    let mr_bytes = mr.finish();
    seg.extend(mr_bytes.iter().rev());
    Some(seg)
}

/// One HT code-block encoded for codestream assembly: the §7.3 cleanup
/// segment, the optional §7.4 + §7.5 refinement segment, and the
/// number of bit-planes the refinement passes carry below the cleanup
/// (`0` or `1`).
#[derive(Debug, Clone)]
pub struct HtEncodedBlock {
    /// HT cleanup segment bytes (§7.1.1 layout).
    pub cleanup: Vec<u8>,
    /// HT refinement segment bytes (`Some` ⇒ `Z_blk = 3`).
    pub refinement: Option<Vec<u8>>,
    /// Bit-planes left to the refinement passes: `0` (cleanup coded
    /// down to the LSB, `Z_blk = 1`) or `1` (`Z_blk = 3`).
    pub beta: u32,
}

/// Encode one HT code-block from full-precision magnitudes + signs.
///
/// With `refine` unset (or unusable) the cleanup pass transports the
/// full magnitudes (`β = 0`, `Z_blk = 1`). With `refine` set the
/// cleanup stops one bit-plane short (`μ_n = mag >> 1`) and a SigProp +
/// MagRef refinement segment carries bit-plane 0 (`β = 1`,
/// `Z_blk = 3`) — falling back to `β = 0` when the block has no
/// cleanup-significant sample to anchor the SigProp neighbourhood scan
/// or when some `mag == 1` sample is unreachable (see
/// [`encode_ht_refinement_segment`]).
pub fn encode_ht_codeblock(
    mag: &[u32],
    sign: &[bool],
    width: usize,
    height: usize,
    refine: bool,
) -> Result<HtEncodedBlock, Error> {
    if refine && mag.iter().any(|&m| m >= 2) {
        if let Some(refinement) = encode_ht_refinement_segment(mag, sign, width, height) {
            let mu: Vec<u32> = mag.iter().map(|&m| m >> 1).collect();
            let cleanup = encode_ht_cleanup_segment(&mu, sign, width, height)?;
            return Ok(HtEncodedBlock {
                cleanup,
                refinement: Some(refinement),
                beta: 1,
            });
        }
    }
    let cleanup = encode_ht_cleanup_segment(mag, sign, width, height)?;
    Ok(HtEncodedBlock {
        cleanup,
        refinement: None,
        beta: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::SubBandOrientation;
    use crate::ht::decode_ht_codeblock;

    fn lcg(state: &mut u32) -> u32 {
        *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *state
    }

    /// Encode `mu`/`sign`, decode through the crate's HT decoder at a
    /// depth where no positional shift applies, and assert bit-exact
    /// magnitudes + signs.
    fn roundtrip(mu: &[u32], sign: &[bool], w: usize, h: usize) -> Vec<u8> {
        let seg = encode_ht_cleanup_segment(mu, sign, w, h).expect("encode");
        let max_mu = mu.iter().copied().max().unwrap_or(0);
        // Nb = S_blk + 1 must reach the deepest μ bit; mb = Nb keeps
        // the §7.6 positioning shift at zero.
        let planes = (32 - max_mu.leading_zeros()).max(1);
        let (block, nb) = decode_ht_codeblock(
            SubBandOrientation::HL,
            w,
            h,
            planes,
            &seg,
            &[],
            1,
            planes - 1,
        )
        .expect("decode own segment");
        assert_eq!(nb, planes);
        for y in 0..h {
            for x in 0..w {
                let c = block.coefficient(x, y);
                let want = mu[x + y * w];
                assert_eq!(c.magnitude, want, "μ at ({x},{y})");
                assert_eq!(c.sigma, want != 0, "σ at ({x},{y})");
                if want != 0 {
                    assert_eq!(c.sign, sign[x + y * w], "sign at ({x},{y})");
                }
            }
        }
        seg
    }

    fn noise_block(w: usize, h: usize, seed: u32, zero_every: u32, max_bits: u32) -> Vec<u32> {
        let mut s = seed;
        (0..w * h)
            .map(|_| {
                let r = lcg(&mut s);
                if zero_every > 0 && r % zero_every == 0 {
                    0
                } else {
                    (r >> 8) & ((1 << max_bits) - 1)
                }
            })
            .collect()
    }

    fn noise_signs(n: usize, seed: u32) -> Vec<bool> {
        let mut s = seed;
        (0..n).map(|_| lcg(&mut s) & 1 == 1).collect()
    }

    #[test]
    fn all_zero_block_round_trips() {
        // Every quad is an AZC quad answered by one MEL symbol; the
        // VLC stream is empty and MagSgn is empty.
        let mu = vec![0u32; 8 * 8];
        let sign = vec![false; 8 * 8];
        roundtrip(&mu, &sign, 8, 8);
    }

    #[test]
    fn single_significant_sample_round_trips() {
        for (x, y) in [(0usize, 0usize), (3, 0), (0, 3), (3, 3), (2, 1)] {
            let mut mu = vec![0u32; 4 * 4];
            let mut sign = vec![false; 4 * 4];
            mu[x + y * 4] = 5;
            sign[x + y * 4] = (x + y) % 2 == 1;
            roundtrip(&mu, &sign, 4, 4);
        }
    }

    #[test]
    fn dense_random_blocks_round_trip() {
        for (i, &(w, h)) in [(4usize, 4usize), (8, 8), (16, 16), (32, 32), (64, 64)]
            .iter()
            .enumerate()
        {
            let mu = noise_block(w, h, 0x1000 + i as u32, 0, 10);
            let sign = noise_signs(w * h, 0x2000 + i as u32);
            roundtrip(&mu, &sign, w, h);
        }
    }

    #[test]
    fn sparse_random_blocks_round_trip() {
        // Long insignificant runs exercise the MEL ladder both ways.
        for (i, &(w, h)) in [(16usize, 16usize), (32, 32), (64, 64)].iter().enumerate() {
            let mu = noise_block(w, h, 0x3000 + i as u32, 3, 8);
            let sign = noise_signs(w * h, 0x4000 + i as u32);
            roundtrip(&mu, &sign, w, h);
        }
        // Very sparse: mostly-zero with a few spikes.
        let mut mu = vec![0u32; 32 * 32];
        let mut s = 0x5A5Au32;
        for _ in 0..12 {
            let at = (lcg(&mut s) as usize) % (32 * 32);
            mu[at] = 1 + (lcg(&mut s) & 0x3FFF);
        }
        let sign = noise_signs(32 * 32, 0x6000);
        roundtrip(&mu, &sign, 32, 32);
    }

    #[test]
    fn odd_dimensions_round_trip() {
        // Odd widths/heights leave quad pad samples; the pad must stay
        // insignificant on both sides.
        for &(w, h) in &[(3usize, 3usize), (5, 7), (7, 5), (1, 1), (1, 9), (9, 1)] {
            let mu = noise_block(w, h, (w * 31 + h) as u32, 4, 6);
            let sign = noise_signs(w * h, (w * 7 + h) as u32);
            roundtrip(&mu, &sign, w, h);
        }
    }

    #[test]
    fn wide_magnitudes_round_trip() {
        // Deep magnitudes drive multi-byte MagSgn values, 0xFF stuffing,
        // and large U-VLC residuals (prefix-5 suffix/extension paths).
        let w = 8;
        let h = 8;
        let mut mu = vec![0u32; w * h];
        let mut s = 0xDEE9_u32;
        for (i, m) in mu.iter_mut().enumerate() {
            *m = if i % 3 == 0 {
                0
            } else {
                1 + (lcg(&mut s) & 0x0FFF_FFFF)
            };
        }
        let sign = noise_signs(w * h, 0xD00D);
        roundtrip(&mu, &sign, w, h);
    }

    #[test]
    fn all_ones_block_round_trips() {
        // μ = 1 everywhere: E_n = 1 = κ on the first line pair, so
        // u_off = 0 and the EMB patterns stay empty; every context
        // value gets exercised as significance saturates.
        let mu = vec![1u32; 16 * 16];
        let sign = noise_signs(16 * 16, 0x1111);
        roundtrip(&mu, &sign, 16, 16);
    }

    #[test]
    fn checkerboards_round_trip() {
        for parity in 0..2usize {
            let w = 12;
            let h = 10;
            let mu: Vec<u32> = (0..w * h)
                .map(|i| {
                    let x = i % w;
                    let y = i / w;
                    if (x + y) % 2 == parity {
                        (i as u32 % 9) + 1
                    } else {
                        0
                    }
                })
                .collect();
            let sign = noise_signs(w * h, 0xC4EC + parity as u32);
            roundtrip(&mu, &sign, w, h);
        }
    }

    #[test]
    fn column_and_row_stripes_round_trip() {
        // Vertical / horizontal stripes drive specific ρ patterns
        // (0x3 / 0x5 / 0xA / 0xC) and their table entries.
        let w = 16;
        let h = 8;
        let vert: Vec<u32> = (0..w * h).map(|i| ((i % w) % 2 == 0) as u32 * 7).collect();
        let horiz: Vec<u32> = (0..w * h).map(|i| ((i / w) % 2 == 0) as u32 * 3).collect();
        let sign = vec![false; w * h];
        roundtrip(&vert, &sign, w, h);
        roundtrip(&horiz, &sign, w, h);
    }

    #[test]
    fn mel_run_flush_is_consistent() {
        // A block ending in a long insignificant run leaves a pending
        // MEL run at flush; the claimed-complete-run close must still
        // decode. Significant top-left corner then zeros.
        let w = 32;
        let h = 32;
        let mut mu = vec![0u32; w * h];
        mu[0] = 9;
        mu[1] = 2;
        let sign = vec![false; w * h];
        roundtrip(&mu, &sign, w, h);
    }

    #[test]
    fn segment_is_compact_for_sparse_content() {
        // Sanity: the HT segment for a mostly-empty block stays small
        // (MEL run coding collapses the insignificant quads).
        let mut mu = vec![0u32; 64 * 64];
        mu[0] = 3;
        let sign = vec![false; 64 * 64];
        let seg = roundtrip(&mu, &sign, 64, 64);
        assert!(seg.len() < 64, "sparse segment blew up: {} B", seg.len());
    }

    // -- §7.4 / §7.5 forward refinement passes --------------------------

    /// Encode with `refine`, decode through the crate's §7.1–§7.6 HT
    /// decoder, and assert bit-exact magnitudes + signs. Returns the
    /// chosen β (1 ⇒ the refinement segment was exercised).
    fn roundtrip_refined(mag: &[u32], sign: &[bool], w: usize, h: usize) -> u32 {
        let enc = encode_ht_codeblock(mag, sign, w, h, true).expect("encode");
        let max_mag = mag.iter().copied().max().unwrap_or(0);
        let planes = (32 - max_mag.leading_zeros()).max(2);
        let (z_blk, s_blk, refinement) = match &enc.refinement {
            // β = 1: cleanup stops at bit-plane 1, S_blk + 2 = Mb.
            Some(r) => (3u8, planes - 2, r.as_slice()),
            // β = 0: cleanup reaches bit-plane 0, S_blk + 1 = Mb.
            None => (1u8, planes - 1, &[][..]),
        };
        let (block, _nb) = decode_ht_codeblock(
            SubBandOrientation::HL,
            w,
            h,
            planes,
            &enc.cleanup,
            refinement,
            z_blk,
            s_blk,
        )
        .expect("decode own segments");
        for y in 0..h {
            for x in 0..w {
                let c = block.coefficient(x, y);
                let want = mag[x + y * w];
                assert_eq!(c.magnitude, want, "mag at ({x},{y}) β={}", enc.beta);
                if want != 0 {
                    assert_eq!(c.sign, sign[x + y * w], "sign at ({x},{y})");
                }
            }
        }
        enc.beta
    }

    /// Dense noise: every mag-1 sample has significant neighbours, so
    /// the refinement mode holds (β = 1) and SigProp + MagRef carry
    /// bit-plane 0 losslessly.
    #[test]
    fn refinement_round_trips_dense_noise() {
        for (w, h, seed, bits) in [
            (4usize, 4usize, 0xAB01u32, 3u32),
            (8, 8, 0xAB02, 5),
            (16, 12, 0xAB03, 8),
            (32, 32, 0xAB04, 12),
            (64, 64, 0xAB05, 16),
            (13, 9, 0xAB06, 6),
        ] {
            let mag: Vec<u32> = noise_block(w, h, seed, 0, bits)
                .iter()
                .map(|&m| m.max(2)) // dense: everything significant
                .collect();
            let sign = noise_signs(w * h, seed ^ 0x5555);
            assert_eq!(roundtrip_refined(&mag, &sign, w, h), 1, "{w}x{h}");
        }
    }

    /// Mixed content with zeros and mag-1 samples adjacent to
    /// significant ones: SigProp must reach them (magnitude *and* sign
    /// recovered from the §7.4 pass).
    #[test]
    fn refinement_round_trips_sigprop_reachable_ones() {
        let w = 16;
        let h = 16;
        let mut mag = vec![0u32; w * h];
        let mut sign = vec![false; w * h];
        // A cross of significant samples with mag-1 satellites.
        for i in 0..w {
            mag[8 * w + i] = 4 + (i as u32 % 3);
            sign[8 * w + i] = i % 2 == 0;
        }
        for i in 0..w {
            mag[7 * w + i] = 1; // neighbours of the significant row
            sign[7 * w + i] = i % 3 == 0;
            mag[9 * w + i] = 1;
            sign[9 * w + i] = i % 3 == 1;
        }
        assert_eq!(roundtrip_refined(&mag, &sign, w, h), 1);
    }

    /// An isolated mag-1 sample (no significant neighbour anywhere)
    /// cannot be reached by SigProp — the encoder falls back to β = 0
    /// and stays lossless.
    #[test]
    fn refinement_falls_back_for_unreachable_one() {
        let w = 8;
        let h = 8;
        let mut mag = vec![0u32; w * h];
        mag[0] = 5; // significant corner
        mag[7 * w + 7] = 1; // isolated LSB-only sample far away
        let sign = vec![false; w * h];
        assert_eq!(roundtrip_refined(&mag, &sign, w, h), 0);
        // All-small blocks (nothing significant at β = 1) also fall
        // back.
        let ones = vec![1u32; w * h];
        assert_eq!(roundtrip_refined(&ones, &vec![false; w * h], w, h), 0);
    }

    /// All-ones bit patterns push the §7.1.5 / §7.1.6 stuffing rules:
    /// SigProp emits long 1-runs (0xFF then 7-bit bytes) and MagRef
    /// alternates the >0x8F / low-seven-ones 7-bit closure.
    #[test]
    fn refinement_stuffing_rules_round_trip() {
        let w = 32;
        let h = 32;
        // Everything significant with odd magnitudes → MagRef stream of
        // all 1s.
        let mag = vec![3u32; w * h];
        let sign = vec![true; w * h];
        assert_eq!(roundtrip_refined(&mag, &sign, w, h), 1);
        // A significant row with odd magnitudes and negative mag-1
        // satellites → SigProp data + sign bits of 1s.
        let mut mag2 = vec![0u32; w * h];
        let mut sign2 = vec![false; w * h];
        for x in 0..w {
            for y in (0..h).step_by(3) {
                mag2[y * w + x] = 5;
                sign2[y * w + x] = true;
            }
            for y in (1..h).step_by(3) {
                mag2[y * w + x] = 1;
                sign2[y * w + x] = true;
            }
        }
        assert_eq!(roundtrip_refined(&mag2, &sign2, w, h), 1);
    }

    /// A black-box-encoder-produced 3x3 LL cleanup segment whose
    /// initial quad-pair takes the §7.3.6 first-line-pair
    /// `s_mel = 0, u_q1 > 2` path with a shared MEL/VLC tail byte —
    /// the shape that exposed the u2 single-bit interleave position
    /// (the bit replaces q2's *prefix step*, so it precedes q1's
    /// suffix). Decodes bit-exactly.
    #[test]
    fn first_line_pair_u2_bit_precedes_suffix_black_box_segment() {
        let seg: Vec<u8> = vec![
            0x09, 0x00, 0x54, 0x8d, 0xf4, 0x59, 0x17, 0xfc, 0x10, 0xbe, 0x97, 0x00,
        ];
        let (block, _nb) =
            decode_ht_codeblock(SubBandOrientation::LL, 3, 3, 9, &seg, &[], 1, 8).expect("decode");
        let want: [(u32, bool); 9] = [
            (5, true),
            (17, false),
            (3, true),
            (1, false),
            (5, false),
            (1, true),
            (15, true),
            (5, false),
            (11, false),
        ];
        for y in 0..3 {
            for x in 0..3 {
                let c = block.coefficient(x, y);
                let (m, sg) = want[y * 3 + x];
                assert_eq!((c.magnitude, c.sign), (m, sg), "({x},{y})");
            }
        }
    }

    /// The same 3x3 content through this crate's own encoder — the
    /// first-line-pair sym=0 interleave and the VLC tail packing must
    /// agree with the decoder bit-for-bit.
    #[test]
    fn first_line_pair_u2_bit_round_trips_own_encoder() {
        let mu = [5u32, 17, 3, 1, 5, 1, 15, 5, 11];
        let sign = [true, false, true, false, false, true, true, false, false];
        roundtrip(&mu, &sign, 3, 3);
    }

    /// High-magnitude content over small / odd quad grids: the exact
    /// family that once dropped an all-ones trailing VLC byte and
    /// misplaced the first-line-pair u2 bit. Sweeps shapes whose last
    /// quad row / column is truncated.
    #[test]
    fn odd_grid_high_energy_round_trips() {
        for (w, h) in [
            (3usize, 3usize),
            (5, 3),
            (3, 5),
            (5, 5),
            (7, 5),
            (9, 7),
            (13, 9),
            (12, 10),
            (6, 5),
            (23, 19),
        ] {
            for bits in [8u32, 10, 12, 14] {
                for seed in 0..20u32 {
                    let mu = noise_block(w, h, 0xDEB0_0000 ^ seed ^ (bits << 8), 0, bits);
                    let sign = noise_signs(w * h, seed);
                    roundtrip(&mu, &sign, w, h);
                }
            }
        }
    }

    /// Randomised sweep across shapes and densities — every block
    /// round-trips bit-exactly whichever β the encoder picks.
    #[test]
    fn refinement_randomised_sweep() {
        let mut seed = 0xC0FF_EE00u32;
        for (w, h) in [(4usize, 4usize), (8, 8), (16, 16), (32, 24), (64, 64)] {
            for zero_every in [0u32, 2, 5] {
                seed ^= 0x9E37_79B9;
                let mag = noise_block(w, h, seed, zero_every, 10);
                let sign = noise_signs(w * h, seed ^ 0xAAAA);
                roundtrip_refined(&mag, &sign, w, h);
            }
        }
    }
}
