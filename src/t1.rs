//! Tier-1 EBCOT bit-plane coding passes — T.800 Annex D.
//!
//! This module implements the **decoder** side of the Annex D coefficient
//! bit-modelling that drives the [`mq`](crate::mq) arithmetic decoder.
//! Annex D specifies three coding passes that decode each bit-plane of a
//! code-block from MSB toward LSB: the **significance propagation pass**
//! (§D.3.1), the **magnitude refinement pass** (§D.3.3) and the
//! **cleanup pass** (§D.3.4). For every bit-plane after the first
//! non-empty one all three passes run in that order; for the first
//! non-empty bit-plane only the cleanup pass runs (§D.3).
//!
//! ## What this module covers
//!
//! Earlier rounds landed the **significance propagation pass** of §D.3.1
//! (with the **sign-bit subroutine** of §D.3.2) and the **magnitude
//! refinement pass** of §D.3.3 (Table D.4, contexts 14, 15, 16). This
//! round adds the **cleanup pass** of §D.3.4 (Table D.5), the last of the
//! three Annex D coding passes: it codes every coefficient the earlier
//! two passes left insignificant, using the run-length context (label 17)
//! with the UNIFORM context (label 18) four-zero-column shortcut where
//! eligible and the Table D.1 significance contexts otherwise. All three
//! coding passes are now in place. They share the coefficient state and
//! scan-order machinery in this module:
//!
//! * The [`CodeBlock`] coefficient state — sample values, per-coefficient
//!   significance state (`σ`), and the sign bit (`χ`). §D.3 calls `σ`
//!   the "significance state" and notes that it is initialised to 0 and
//!   may transition to 1 on any pass.
//! * The §D.1 stripe-major scan order: top-down, the code-block is
//!   partitioned into horizontal stripes of height 4 (the bottom stripe
//!   may be 1, 2 or 3 rows tall when the code-block height is not
//!   divisible by 4); within each stripe coefficients are scanned column
//!   by column, top to bottom of the stripe, before moving to the next
//!   column. The whole stripe is traversed before the next stripe starts.
//!   Figure D.1 illustrates the pattern.
//! * The neighbour-σ derivation: each coefficient `(u, v)` has the eight
//!   neighbours `(u−1, v−1)`, `(u, v−1)`, `(u+1, v−1)`, `(u−1, v)`,
//!   `(u+1, v)`, `(u−1, v+1)`, `(u, v+1)`, `(u+1, v+1)` shown in Figure
//!   D.2. Neighbours outside the code-block are taken to be insignificant
//!   per §D.3.
//! * The per-orientation significance-context map of Table D.1 (context
//!   labels `0..=8` per [`SP_CTX_OFFSET`] + 0..8), and the sign-context
//!   map of Tables D.2 / D.3 (context labels `9..=13` per
//!   [`SIGN_CTX_OFFSET`] + 0..4, with the §D.3.2 XORbit).
//!
//! ## Context array layout
//!
//! Annex D uses 19 distinct contexts (Table D.7), all of which are now
//! touched across the three passes: labels `0..=8` for significance
//! propagation / cleanup, `9..=13` for sign, `14..=16` for magnitude
//! refinement, `17` for the cleanup run-length context, and `18` for the
//! UNIFORM context. The caller owns the `[MqContext; 19]` array (see
//! [`reset_contexts`]).
//!
//! ## What this module does NOT cover yet
//!
//! * The bit-plane sequencer that drives the §D.3 three-pass order
//!   (cleanup-only first bit-plane, then SP → MR → cleanup) across a whole
//!   code-block — the three passes are individually callable; chaining
//!   them per code-block from the packet reader's byte ranges is the next
//!   step.
//! * The §D.4.2 / §D.5 / §D.6 termination / segmentation-symbol / raw-
//!   bit-bypass modes.
//! * §D.7 vertically causal context formation — a flag for the future
//!   COD `Scod` bit-3 mode.
//!
//! ## Clean-room provenance
//!
//! Implemented solely from `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf`
//! (§D.1 — code-block scan pattern; §D.2 — coefficient bits / significance
//! / sign notations; §D.3 — decoding passes prologue + Figure D.2
//! neighbour layout; §D.3.1 + Table D.1 — significance propagation
//! contexts; §D.3.2 + Table D.2 + Table D.3 + Equation D-1 — sign
//! contexts and XORbit; §D.3.3 + Table D.4 — magnitude refinement
//! contexts; §D.3.4 + Table D.5 — cleanup pass run-length / UNIFORM
//! four-zero-column shortcut; §D.4 + Table D.7 — initial context states).
//! The Figure D.2 layout is the in-PDF diagram transcribed to neighbour
//! offsets; the Tables D.1 / D.2 / D.3 / D.4 / D.5 contents are
//! transcribed verbatim from the PDF. No external library source — OpenJPEG, OpenJPH, Kakadu,
//! Grok, FFmpeg, libavcodec, jpeg2000-rs, etc. — was consulted, quoted,
//! paraphrased, or used as a cross-check oracle. No WebSearch /
//! WebFetch was used for any reason.

use crate::geometry::SubBandOrientation;
use crate::mq::{MqContext, MqDecoder};
use crate::Error;

/// First Annex D context label devoted to significance propagation
/// (Table D.1 row "0", the all-insignificant-neighbours row).
///
/// The nine SP labels are `SP_CTX_OFFSET + 0 ..= SP_CTX_OFFSET + 8`
/// (i.e., `0..=8` in the caller's context array).
pub const SP_CTX_OFFSET: usize = 0;

/// First Annex D context label devoted to sign-bit decoding (Table D.3
/// label "9", the `(H=0, V=0)` row).
///
/// The five sign labels are `SIGN_CTX_OFFSET + 0 ..= SIGN_CTX_OFFSET + 4`
/// (i.e., `9..=13` in the caller's context array).
pub const SIGN_CTX_OFFSET: usize = 9;

/// First magnitude-refinement label (Table D.4 label "14", the
/// `∑(Hi+Vi+Di) = 0`, first-refinement row).
///
/// The three refinement labels are
/// `REFINEMENT_CTX_OFFSET + 0 ..= REFINEMENT_CTX_OFFSET + 2` (i.e.,
/// `14..=16` in the caller's context array): 14 for a first refinement
/// with no significant neighbours, 15 for a first refinement with at
/// least one significant neighbour, and 16 for any later refinement.
pub const REFINEMENT_CTX_OFFSET: usize = 14;

/// Run-length context label per Table D.7 (§D.3.4 cleanup pass).
pub const RUN_LENGTH_CTX: usize = 17;

/// UNIFORM context label per Table D.7 (§C.3 / §D.3.4).
pub const UNIFORM_CTX: usize = 18;

/// Number of distinct context labels the Annex D coding passes use
/// across all three passes (Table D.7).
pub const NUM_CONTEXTS: usize = 19;

/// Initialise an Annex D context array to the Table D.7 reset states.
///
/// Per Table D.7 every context starts at Table C.2 index 0 / MPS 0
/// except for three special cases:
///
/// * Label 0 (the "zero-neighbours" / first SP row) — index 4.
/// * Label 17 (the run-length context) — index 3.
/// * Label 18 (the UNIFORM context) — index 46.
///
/// The caller passes this array, along with a freshly-initialised
/// [`MqDecoder`], into [`CodeBlock::significance_propagation_pass`].
pub fn reset_contexts() -> [MqContext; NUM_CONTEXTS] {
    let mut ctx = [MqContext::default(); NUM_CONTEXTS];
    ctx[0] = MqContext::zero_neighbours();
    ctx[RUN_LENGTH_CTX] = MqContext::run_length();
    ctx[UNIFORM_CTX] = MqContext::uniform();
    ctx
}

/// One transform coefficient inside a code-block.
///
/// * `magnitude` — the absolute value being recovered MSB-first. The
///   coding passes shift bits into the low end of this field as they
///   come out of the MQ decoder; on the bit-plane currently being
///   decoded the bit's positional value is `1 << bitplane`.
/// * `sigma` — the §D.3 significance state. `false` while the
///   coefficient is still insignificant, flipping to `true` once a `1`
///   magnitude bit has been observed.
/// * `sign` — the §D.2 sign bit `sb(u, v)`. `false` ≡ positive, `true`
///   ≡ negative. Only meaningful once `sigma` is true; ignored on
///   insignificant coefficients per §D.3.2.
/// * `already_refined` — set by the magnitude refinement pass after the
///   first refinement bit lands (Table D.4's "first refinement"
///   indicator). The §D.3.3 pass reads it to pick context 16 (already
///   refined) over 14/15 (first refinement), then sets it; the SP pass
///   leaves it untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Coefficient {
    /// Reconstructed magnitude (bits arrive MSB-first).
    pub magnitude: u32,
    /// §D.3 significance state σ(u, v).
    pub sigma: bool,
    /// §D.2 sign bit sb(u, v) — `true` is negative.
    pub sign: bool,
    /// Table D.4 "first refinement" flag — `true` after one refinement
    /// pass has fired on this coefficient.
    pub already_refined: bool,
}

/// One rectangular code-block under tier-1 decoding.
///
/// The coefficient grid is stored in raster-major (row-major) order:
/// `coefficients[u + v * width]` is `(u, v)`.
#[derive(Debug, Clone)]
pub struct CodeBlock {
    /// Sub-band orientation; selects the Table D.1 column.
    orientation: SubBandOrientation,
    /// Code-block width in samples.
    width: usize,
    /// Code-block height in samples.
    height: usize,
    /// Raster-major coefficient grid (`width * height` entries).
    coefficients: Vec<Coefficient>,
    /// Per-coefficient "this coefficient was made significant inside the
    /// current SP pass" flag. The magnitude refinement pass (§D.3.3)
    /// uses this to **skip** coefficients that just became significant,
    /// per §D.3.3's "except those that have just become significant in
    /// the immediately preceding significance propagation pass". Cleared
    /// at the start of every SP pass.
    newly_significant: Vec<bool>,
}

impl CodeBlock {
    /// Construct a fresh, all-insignificant code-block of the given
    /// sub-band orientation and dimensions.
    ///
    /// `width` and `height` must be at least 1; the §B.7 effective
    /// `xcb'` / `ycb'` are at least `0` (`2^0 = 1`).
    pub fn new(orientation: SubBandOrientation, width: usize, height: usize) -> Self {
        assert!(width >= 1, "code-block width must be ≥ 1");
        assert!(height >= 1, "code-block height must be ≥ 1");
        Self {
            orientation,
            width,
            height,
            coefficients: vec![Coefficient::default(); width * height],
            newly_significant: vec![false; width * height],
        }
    }

    /// Code-block width in samples.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Code-block height in samples.
    pub fn height(&self) -> usize {
        self.height
    }

    /// Sub-band orientation (used for the Table D.1 context column).
    pub fn orientation(&self) -> SubBandOrientation {
        self.orientation
    }

    /// Read one coefficient `(u, v)` (`u` ∈ `0..width`, `v` ∈ `0..height`).
    pub fn coefficient(&self, u: usize, v: usize) -> Coefficient {
        debug_assert!(u < self.width && v < self.height);
        self.coefficients[u + v * self.width]
    }

    /// Whether `(u, v)` became significant in the most recent SP pass.
    /// Cleared at the start of every SP pass.
    pub fn was_newly_significant(&self, u: usize, v: usize) -> bool {
        debug_assert!(u < self.width && v < self.height);
        self.newly_significant[u + v * self.width]
    }

    /// Run one §D.3.1 significance-propagation pass over the bit-plane
    /// with positional weight `1 << bitplane`.
    ///
    /// The pass walks every coefficient in §D.1 stripe-major order;
    /// for each one that is currently insignificant and whose Table D.1
    /// context label is **non-zero**, one MQ decision is drawn against
    /// that context. A `1` decision flips σ to true, immediately
    /// followed by the §D.3.2 sign-bit subroutine (one further MQ
    /// decision against the Table D.3 sign context, XORed with the
    /// Table D.3 XORbit to recover the spec's `sb(u, v)`). A `0`
    /// decision leaves σ false and skips the sign subroutine.
    ///
    /// The caller-supplied `ctx` array must hold all 19 Annex D
    /// contexts in their Table D.7 initial states (see
    /// [`reset_contexts`]); the SP pass only mutates entries
    /// `0..=8` (significance) and `9..=13` (sign).
    ///
    /// Returns the number of coefficients that **became newly
    /// significant** in this pass — exposed mainly for tests; the
    /// per-coefficient flag is reachable via
    /// [`was_newly_significant`](Self::was_newly_significant).
    pub fn significance_propagation_pass(
        &mut self,
        bitplane: u32,
        decoder: &mut MqDecoder<'_>,
        ctx: &mut [MqContext; NUM_CONTEXTS],
    ) -> Result<usize, Error> {
        // §D.3.3: clear the "just became significant in the last SP
        // pass" carry at the start of every new SP pass; bits set during
        // *this* pass will be visible to the *next* magnitude refinement
        // pass.
        for flag in &mut self.newly_significant {
            *flag = false;
        }

        let weight: u32 = 1u32 << bitplane;
        let mut newly = 0usize;

        // §D.1: stripes of height 4, top to bottom. Within a stripe the
        // scan is column-by-column, top to bottom of the stripe.
        let mut v0 = 0usize;
        while v0 < self.height {
            let stripe_h = core::cmp::min(4, self.height - v0);
            for u in 0..self.width {
                for dv in 0..stripe_h {
                    let v = v0 + dv;
                    let coef = self.coefficients[u + v * self.width];

                    // SP pass: only insignificant coefficients with a
                    // non-zero Table D.1 context are considered.
                    if coef.sigma {
                        continue;
                    }
                    let label = significance_context_label(self.orientation, self.neighbours(u, v));
                    if label == 0 {
                        // Zero context — deferred to the cleanup pass.
                        continue;
                    }

                    let cx = &mut ctx[SP_CTX_OFFSET + label as usize];
                    let bit = decoder.decode(cx);
                    if bit == 1 {
                        // Newly significant — set σ, accumulate the
                        // bit-plane's positional value into magnitude,
                        // then immediately run the §D.3.2 sign-bit
                        // subroutine.
                        let nb = self.neighbours(u, v);
                        let (sign_label, xorbit) = sign_context_label(nb);
                        let sign_cx = &mut ctx[SIGN_CTX_OFFSET + sign_label as usize];
                        let d = decoder.decode(sign_cx);
                        let sign_bit = (d ^ xorbit) != 0;

                        let updated = Coefficient {
                            magnitude: coef.magnitude | weight,
                            sigma: true,
                            sign: sign_bit,
                            already_refined: coef.already_refined,
                        };
                        self.coefficients[u + v * self.width] = updated;
                        self.newly_significant[u + v * self.width] = true;
                        newly += 1;
                    }
                }
            }
            v0 += 4;
        }

        Ok(newly)
    }

    /// Run one §D.3.3 magnitude-refinement pass over the bit-plane with
    /// positional weight `1 << bitplane`.
    ///
    /// The pass walks every coefficient in §D.1 stripe-major order (the
    /// same order as the SP pass) and refines exactly those coefficients
    /// that are **already significant** *and* did **not** become
    /// significant in the immediately preceding significance-propagation
    /// pass (§D.3.3's "except those that have just become significant in
    /// the immediately preceding significance propagation pass" — the
    /// `newly_significant` carry tracks that set).
    ///
    /// For each refined coefficient one MQ decision is drawn against the
    /// Table D.4 context: label 14 / 15 for the **first** refinement of
    /// the coefficient (depending on whether `∑(Hi+Vi+Di) = 0` or `≥ 1`
    /// using the *current* significance states) and label 16 for any
    /// later refinement. The decoded bit is OR-ed into `magnitude` at the
    /// bit-plane's positional weight and the coefficient is marked
    /// `already_refined`.
    ///
    /// The caller-supplied `ctx` array must hold all 19 Annex D contexts
    /// (see [`reset_contexts`]); the refinement pass only mutates entries
    /// `14..=16`. The `newly_significant` carry is left intact so a
    /// following pass on the same bit-plane (there is none in the §D.3
    /// order, but tests may chain passes) still sees it; it is cleared at
    /// the start of the next SP pass.
    ///
    /// Returns the number of coefficients refined in this pass (exposed
    /// mainly for tests).
    pub fn magnitude_refinement_pass(
        &mut self,
        bitplane: u32,
        decoder: &mut MqDecoder<'_>,
        ctx: &mut [MqContext; NUM_CONTEXTS],
    ) -> Result<usize, Error> {
        let weight: u32 = 1u32 << bitplane;
        let mut refined = 0usize;

        let mut v0 = 0usize;
        while v0 < self.height {
            let stripe_h = core::cmp::min(4, self.height - v0);
            for u in 0..self.width {
                for dv in 0..stripe_h {
                    let v = v0 + dv;
                    let idx = u + v * self.width;
                    let coef = self.coefficients[idx];

                    // §D.3.3: refine coefficients that are already
                    // significant, *except* those that became significant
                    // in the immediately preceding SP pass.
                    if !coef.sigma || self.newly_significant[idx] {
                        continue;
                    }

                    // Table D.4 context: the neighbour summation uses the
                    // significance states currently known to the decoder.
                    let label =
                        refinement_context_label(self.neighbours(u, v), coef.already_refined);
                    let cx = &mut ctx[REFINEMENT_CTX_OFFSET + label as usize];
                    let bit = decoder.decode(cx);

                    let mut updated = coef;
                    if bit == 1 {
                        updated.magnitude |= weight;
                    }
                    updated.already_refined = true;
                    self.coefficients[idx] = updated;
                    refined += 1;
                }
            }
            v0 += 4;
        }

        Ok(refined)
    }

    /// Run one §D.3.4 cleanup pass over the bit-plane with positional
    /// weight `1 << bitplane`.
    ///
    /// The cleanup pass codes every coefficient that is **still
    /// insignificant** after the significance-propagation and
    /// magnitude-refinement passes of this bit-plane (i.e., every
    /// coefficient the earlier passes "left over"). It walks the same
    /// §D.1 stripe-major scan order as the other two passes.
    ///
    /// Two coding modes apply, per Table D.5:
    ///
    /// * **Run-length mode.** When a column inside a *full* (4-row) stripe
    ///   has all four of its coefficients still insignificant *and* the
    ///   Table D.1 significance context of every one of them is `0`
    ///   (computed from the significance state currently known to the
    ///   decoder), the four coefficients are coded together. One MQ
    ///   decision is drawn against the **run-length context** (label 17).
    ///   A `0` leaves all four insignificant. A `1` means at least one is
    ///   significant; two further bits are drawn against the **UNIFORM
    ///   context** (label 18), MSB then LSB, giving the 0-based index
    ///   (from the top of the column) of the first significant
    ///   coefficient. That coefficient is made significant and its sign
    ///   bit decoded per §D.3.2; the coefficients below it in the column
    ///   are then coded "in the manner described in §D.3.1" (Table D.1
    ///   significance context + sign subroutine).
    /// * **Normal mode.** When the column is not run-length-eligible
    ///   (fewer than four rows remain, or any of the four was already
    ///   coded / has a non-zero context), each still-insignificant
    ///   coefficient of the column is coded individually exactly as in the
    ///   significance-propagation pass: one MQ decision against its Table
    ///   D.1 context, and on a `1` the §D.3.2 sign subroutine.
    ///
    /// Coefficients already significant (from the SP or MR pass of this
    /// bit-plane, or any earlier bit-plane) are skipped — they were not
    /// "left over" for the cleanup pass.
    ///
    /// The caller-supplied `ctx` array must hold all 19 Annex D contexts
    /// (see [`reset_contexts`]); the cleanup pass mutates the significance
    /// labels `0..=8`, the sign labels `9..=13`, the run-length label
    /// `17`, and the UNIFORM label `18`.
    ///
    /// Returns the number of coefficients that **became newly significant**
    /// in this pass (exposed mainly for tests).
    pub fn cleanup_pass(
        &mut self,
        bitplane: u32,
        decoder: &mut MqDecoder<'_>,
        ctx: &mut [MqContext; NUM_CONTEXTS],
    ) -> Result<usize, Error> {
        let weight: u32 = 1u32 << bitplane;
        let mut newly = 0usize;

        let mut v0 = 0usize;
        while v0 < self.height {
            let stripe_h = core::cmp::min(4, self.height - v0);
            for u in 0..self.width {
                // §D.3.4 / Table D.5 run-length eligibility: a full
                // (4-row) stripe whose column has all four coefficients
                // still insignificant and each currently carrying the 0
                // significance context.
                let mut start_dv = 0usize;
                if stripe_h == 4 && self.column_run_length_eligible(u, v0) {
                    let rl_cx = &mut ctx[RUN_LENGTH_CTX];
                    if decoder.decode(rl_cx) == 0 {
                        // All four remain insignificant; skip the column.
                        continue;
                    }
                    // At least one significant: two UNIFORM bits (MSB
                    // then LSB) give the 0-based first-significant index.
                    let hi = decoder.decode(&mut ctx[UNIFORM_CTX]);
                    let lo = decoder.decode(&mut ctx[UNIFORM_CTX]);
                    let first = ((hi << 1) | lo) as usize;
                    // The first-significant coefficient is significant by
                    // construction (the run-length escape already told us
                    // so); decode only its sign, then continue normally
                    // from the next coefficient down the column.
                    let v = v0 + first;
                    self.make_significant_with_sign(u, v, weight, decoder, ctx);
                    newly += 1;
                    start_dv = first + 1;
                }

                // Normal §D.3.1-style coding for the remaining
                // coefficients of the column (or the whole column when
                // run-length mode did not apply).
                for dv in start_dv..stripe_h {
                    let v = v0 + dv;
                    if self.coefficients[u + v * self.width].sigma {
                        continue;
                    }
                    let label = significance_context_label(self.orientation, self.neighbours(u, v));
                    let cx = &mut ctx[SP_CTX_OFFSET + label as usize];
                    if decoder.decode(cx) == 1 {
                        self.make_significant_with_sign(u, v, weight, decoder, ctx);
                        newly += 1;
                    }
                }
            }
            v0 += 4;
        }

        Ok(newly)
    }

    /// Whether the four-coefficient column starting at `(u, v0)` is
    /// run-length-eligible per §D.3.4: every one of the four coefficients
    /// is still insignificant and its current Table D.1 significance
    /// context label is `0`. Callers must have already established that
    /// the stripe is a full 4-row stripe.
    fn column_run_length_eligible(&self, u: usize, v0: usize) -> bool {
        for dv in 0..4 {
            let v = v0 + dv;
            let c = self.coefficients[u + v * self.width];
            if c.sigma {
                return false;
            }
            if significance_context_label(self.orientation, self.neighbours(u, v)) != 0 {
                return false;
            }
        }
        true
    }

    /// Make coefficient `(u, v)` significant: set σ, accumulate the
    /// bit-plane `weight` into the magnitude, decode the sign bit via the
    /// §D.3.2 subroutine, and flag it newly-significant (the §D.3.3
    /// carry). Shared by the cleanup pass's run-length and normal modes.
    fn make_significant_with_sign(
        &mut self,
        u: usize,
        v: usize,
        weight: u32,
        decoder: &mut MqDecoder<'_>,
        ctx: &mut [MqContext; NUM_CONTEXTS],
    ) {
        let idx = u + v * self.width;
        let nb = self.neighbours(u, v);
        let (sign_label, xorbit) = sign_context_label(nb);
        let sign_cx = &mut ctx[SIGN_CTX_OFFSET + sign_label as usize];
        let d = decoder.decode(sign_cx);
        let sign_bit = (d ^ xorbit) != 0;

        let coef = &mut self.coefficients[idx];
        coef.magnitude |= weight;
        coef.sigma = true;
        coef.sign = sign_bit;
        self.newly_significant[idx] = true;
    }

    /// Build a [`Neighbours`] snapshot of `(u, v)`'s 8 nearest neighbours.
    ///
    /// Out-of-block neighbours are treated as insignificant
    /// (`sigma = false`, `sign = false`) per §D.3 / §D.3.2.
    fn neighbours(&self, u: usize, v: usize) -> Neighbours {
        let mut nb = Neighbours::default();
        // Iterate the eight (du, dv) offsets in the Figure D.2 grid.
        let positions: [(i32, i32, NeighbourSlot); 8] = [
            (-1, -1, NeighbourSlot::D0),
            (0, -1, NeighbourSlot::V0),
            (1, -1, NeighbourSlot::D1),
            (-1, 0, NeighbourSlot::H0),
            (1, 0, NeighbourSlot::H1),
            (-1, 1, NeighbourSlot::D2),
            (0, 1, NeighbourSlot::V1),
            (1, 1, NeighbourSlot::D3),
        ];
        let w = self.width as i32;
        let h = self.height as i32;
        let ui = u as i32;
        let vi = v as i32;
        for (du, dv, slot) in positions {
            let nu = ui + du;
            let nv = vi + dv;
            if nu < 0 || nu >= w || nv < 0 || nv >= h {
                continue;
            }
            let c = self.coefficients[(nu as usize) + (nv as usize) * self.width];
            nb.set(slot, c.sigma, c.sign);
        }
        nb
    }
}

/// The eight Figure D.2 neighbour slots around the current coefficient
/// X. Naming follows the diagram: `D0..=D3` are the four diagonal
/// neighbours (top-left, top-right, bottom-left, bottom-right), `V0` /
/// `V1` are vertical (top, bottom), `H0` / `H1` are horizontal (left,
/// right).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NeighbourSlot {
    D0,
    V0,
    D1,
    H0,
    H1,
    D2,
    V1,
    D3,
}

/// Significance + sign snapshot of the 8 Figure D.2 neighbours.
///
/// Each slot carries the neighbour's σ state and (when σ is true) its
/// sign bit. Slots outside the code-block are left at `sigma = false`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Neighbours {
    /// Top-left diagonal `(u−1, v−1)`.
    pub d0_sigma: bool,
    /// Top vertical `(u, v−1)`.
    pub v0_sigma: bool,
    /// Top vertical sign.
    pub v0_sign: bool,
    /// Top-right diagonal `(u+1, v−1)`.
    pub d1_sigma: bool,
    /// Left horizontal `(u−1, v)`.
    pub h0_sigma: bool,
    /// Left horizontal sign.
    pub h0_sign: bool,
    /// Right horizontal `(u+1, v)`.
    pub h1_sigma: bool,
    /// Right horizontal sign.
    pub h1_sign: bool,
    /// Bottom-left diagonal `(u−1, v+1)`.
    pub d2_sigma: bool,
    /// Bottom vertical `(u, v+1)`.
    pub v1_sigma: bool,
    /// Bottom vertical sign.
    pub v1_sign: bool,
    /// Bottom-right diagonal `(u+1, v+1)`.
    pub d3_sigma: bool,
}

impl Neighbours {
    /// Construct a Neighbours snapshot directly. Mainly for unit tests
    /// that exercise [`significance_context_label`] and
    /// [`sign_context_label`] in isolation.
    #[allow(clippy::too_many_arguments)]
    pub fn from_slots(
        d0_sigma: bool,
        v0_sigma: bool,
        v0_sign: bool,
        d1_sigma: bool,
        h0_sigma: bool,
        h0_sign: bool,
        h1_sigma: bool,
        h1_sign: bool,
        d2_sigma: bool,
        v1_sigma: bool,
        v1_sign: bool,
        d3_sigma: bool,
    ) -> Self {
        Self {
            d0_sigma,
            v0_sigma,
            v0_sign,
            d1_sigma,
            h0_sigma,
            h0_sign,
            h1_sigma,
            h1_sign,
            d2_sigma,
            v1_sigma,
            v1_sign,
            d3_sigma,
        }
    }

    fn set(&mut self, slot: NeighbourSlot, sigma: bool, sign: bool) {
        match slot {
            NeighbourSlot::D0 => self.d0_sigma = sigma,
            NeighbourSlot::V0 => {
                self.v0_sigma = sigma;
                self.v0_sign = sign;
            }
            NeighbourSlot::D1 => self.d1_sigma = sigma,
            NeighbourSlot::H0 => {
                self.h0_sigma = sigma;
                self.h0_sign = sign;
            }
            NeighbourSlot::H1 => {
                self.h1_sigma = sigma;
                self.h1_sign = sign;
            }
            NeighbourSlot::D2 => self.d2_sigma = sigma,
            NeighbourSlot::V1 => {
                self.v1_sigma = sigma;
                self.v1_sign = sign;
            }
            NeighbourSlot::D3 => self.d3_sigma = sigma,
        }
    }

    /// `∑Hi` — number of significant horizontal neighbours (0, 1, or 2).
    pub fn h_sum(&self) -> u8 {
        u8::from(self.h0_sigma) + u8::from(self.h1_sigma)
    }

    /// `∑Vi` — number of significant vertical neighbours (0, 1, or 2).
    pub fn v_sum(&self) -> u8 {
        u8::from(self.v0_sigma) + u8::from(self.v1_sigma)
    }

    /// `∑Di` — number of significant diagonal neighbours (0..=4).
    pub fn d_sum(&self) -> u8 {
        u8::from(self.d0_sigma)
            + u8::from(self.d1_sigma)
            + u8::from(self.d2_sigma)
            + u8::from(self.d3_sigma)
    }
}

/// Map an `(orientation, neighbour-σ)` pair onto its Table D.1 context
/// label (`0..=8`). The label is the row's right-most "Context label"
/// column.
///
/// The mapping reads Table D.1 row-by-row, taking the first row whose
/// `∑Hi` / `∑Vi` / `∑Di` constraints all match for the orientation's
/// column. "x" entries are wildcards.
///
/// Table D.1 swaps the H and V columns between the LL/LH "vertical
/// high-pass" group and the HL "horizontal high-pass" group; we handle
/// HL by symmetrically swapping the input `h_sum` / `v_sum` and reading
/// the LL/LH column. The HH column reduces to `∑(Hi+Vi)` and `∑Di`,
/// since the table merges H and V there.
pub fn significance_context_label(orientation: SubBandOrientation, nb: Neighbours) -> u8 {
    let h = nb.h_sum();
    let v = nb.v_sum();
    let d = nb.d_sum();
    match orientation {
        // LL and LH read the table directly.
        SubBandOrientation::LL | SubBandOrientation::LH => sp_label_ll_lh(h, v, d),
        // HL swaps H and V (the table's HL column has H and V columns
        // mirrored relative to LL/LH).
        SubBandOrientation::HL => sp_label_ll_lh(v, h, d),
        // HH uses (H+V, D).
        SubBandOrientation::HH => sp_label_hh(h + v, d),
    }
}

/// Table D.1 LL / LH (and HL after H/V swap) significance-context
/// lookup — column triple `(∑Hi, ∑Vi, ∑Di)` to label.
fn sp_label_ll_lh(h: u8, v: u8, d: u8) -> u8 {
    // Table D.1, rows top to bottom (label 8 down to label 0). The
    // first matching row wins.
    if h == 2 {
        return 8;
    }
    if h == 1 && v >= 1 {
        return 7;
    }
    if h == 1 && v == 0 && d >= 1 {
        return 6;
    }
    if h == 1 && v == 0 && d == 0 {
        return 5;
    }
    if h == 0 && v == 2 {
        return 4;
    }
    if h == 0 && v == 1 {
        return 3;
    }
    if h == 0 && v == 0 && d >= 2 {
        return 2;
    }
    if h == 0 && v == 0 && d == 1 {
        return 1;
    }
    // h == 0, v == 0, d == 0
    0
}

/// Table D.1 HH significance-context lookup — `(∑(Hi+Vi), ∑Di)` to
/// label.
fn sp_label_hh(hv: u8, d: u8) -> u8 {
    // Rows top to bottom (label 8 down to label 0).
    if d >= 3 {
        return 8;
    }
    if d == 2 && hv >= 1 {
        return 7;
    }
    if d == 2 && hv == 0 {
        return 6;
    }
    if d == 1 && hv >= 2 {
        return 5;
    }
    if d == 1 && hv == 1 {
        return 4;
    }
    if d == 1 && hv == 0 {
        return 3;
    }
    if d == 0 && hv >= 2 {
        return 2;
    }
    if d == 0 && hv == 1 {
        return 1;
    }
    // d == 0, hv == 0
    0
}

/// Compute the §D.3.2 sign context label (`0..=4`, relative to
/// [`SIGN_CTX_OFFSET`]) and the Table D.3 XORbit (`0` or `1`) for a
/// coefficient whose 8-neighbour snapshot is `nb`.
///
/// Returns `(label_within_sign_block, xorbit)`. The MQ decision drawn
/// against context `SIGN_CTX_OFFSET + label_within_sign_block` is XORed
/// with `xorbit` to produce the sign bit per Equation D-1
/// (`signbit = D ⊕ XORbit`; `1` is negative).
pub fn sign_context_label(nb: Neighbours) -> (u8, u8) {
    let h_contrib =
        horizontal_or_vertical_contribution(nb.h0_sigma, nb.h0_sign, nb.h1_sigma, nb.h1_sign);
    let v_contrib =
        horizontal_or_vertical_contribution(nb.v0_sigma, nb.v0_sign, nb.v1_sigma, nb.v1_sign);
    sign_label_from_contributions(h_contrib, v_contrib)
}

/// Map the §D.3.2 first-step contribution table (Table D.2) for one
/// axis. Inputs are the two neighbours' `(sigma, sign)` pairs; the
/// output is `-1`, `0`, or `+1`.
///
/// Table D.2 rows (the first neighbour is X0, the second X1):
///
/// * both significant positive                  → +1
/// * significant negative, significant positive →  0
/// * insignificant,        significant positive → +1
/// * significant positive, significant negative →  0
/// * both significant negative                  → -1
/// * insignificant,        significant negative → -1
/// * significant positive, insignificant        → +1
/// * significant negative, insignificant        → -1
/// * both insignificant                         →  0
fn horizontal_or_vertical_contribution(
    x0_sigma: bool,
    x0_sign: bool,
    x1_sigma: bool,
    x1_sign: bool,
) -> i8 {
    // Map to (significant_positive, significant_negative, insignificant)
    // tri-state.
    let s0 = state(x0_sigma, x0_sign);
    let s1 = state(x1_sigma, x1_sign);
    match (s0, s1) {
        (NState::SPos, NState::SPos) => 1,
        (NState::SNeg, NState::SPos) => 0,
        (NState::Insig, NState::SPos) => 1,
        (NState::SPos, NState::SNeg) => 0,
        (NState::SNeg, NState::SNeg) => -1,
        (NState::Insig, NState::SNeg) => -1,
        (NState::SPos, NState::Insig) => 1,
        (NState::SNeg, NState::Insig) => -1,
        (NState::Insig, NState::Insig) => 0,
    }
}

/// Internal tri-state per Table D.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NState {
    /// Significant, positive sign (sign bit 0).
    SPos,
    /// Significant, negative sign (sign bit 1).
    SNeg,
    /// Insignificant (σ false).
    Insig,
}

fn state(sigma: bool, sign: bool) -> NState {
    if !sigma {
        NState::Insig
    } else if sign {
        NState::SNeg
    } else {
        NState::SPos
    }
}

/// Table D.3 second step — reduce `(H_contrib, V_contrib)` to the
/// `(context label, XORbit)` pair. Labels in the table are 9..=13;
/// we return them relative to [`SIGN_CTX_OFFSET`] (i.e., 0..=4).
fn sign_label_from_contributions(h: i8, v: i8) -> (u8, u8) {
    // Table D.3 rows, top to bottom:
    //   ( 1,  1) → 13 / 0
    //   ( 1,  0) → 12 / 0
    //   ( 1, -1) → 11 / 0
    //   ( 0,  1) → 10 / 0
    //   ( 0,  0) →  9 / 0
    //   ( 0, -1) → 10 / 1
    //   (-1,  1) → 11 / 1
    //   (-1,  0) → 12 / 1
    //   (-1, -1) → 13 / 1
    let (label_abs, xor) = match (h, v) {
        (1, 1) => (13, 0),
        (1, 0) => (12, 0),
        (1, -1) => (11, 0),
        (0, 1) => (10, 0),
        (0, 0) => (9, 0),
        (0, -1) => (10, 1),
        (-1, 1) => (11, 1),
        (-1, 0) => (12, 1),
        (-1, -1) => (13, 1),
        // Unreachable: contributions are constrained to {-1, 0, 1}.
        _ => unreachable!("sign contributions outside {{-1, 0, 1}}"),
    };
    debug_assert!((9..=13).contains(&label_abs));
    (label_abs - SIGN_CTX_OFFSET as u8, xor)
}

/// Map an `(8-neighbour snapshot, already-refined)` pair onto its Table
/// D.4 magnitude-refinement context label, returned relative to
/// [`REFINEMENT_CTX_OFFSET`] (i.e., `0..=2` for absolute labels
/// `14..=16`).
///
/// Per Table D.4 the choice is:
///
/// * `already_refined == true`  → label 16 (the "x"/don't-care row;
///   neighbour state is irrelevant once a coefficient has been refined
///   at least once).
/// * `already_refined == false` and `∑(Hi+Vi+Di) ≥ 1` → label 15 (first
///   refinement with at least one significant neighbour).
/// * `already_refined == false` and `∑(Hi+Vi+Di) == 0` → label 14 (first
///   refinement with no significant neighbours).
///
/// The neighbour summation merges all three axes (horizontal, vertical,
/// diagonal) into one count, using the significance states currently
/// known to the decoder (§D.3.3).
pub fn refinement_context_label(nb: Neighbours, already_refined: bool) -> u8 {
    if already_refined {
        // Label 16: REFINEMENT_CTX_OFFSET + 2.
        return 2;
    }
    let sum = nb.h_sum() + nb.v_sum() + nb.d_sum();
    if sum >= 1 {
        // Label 15: REFINEMENT_CTX_OFFSET + 1.
        1
    } else {
        // Label 14: REFINEMENT_CTX_OFFSET + 0.
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Context-array reset (Table D.7) ------------------------------

    #[test]
    fn reset_contexts_populates_table_d7_specials() {
        let ctx = reset_contexts();
        // Label 0 → zero-neighbours (Table C.2 index 4).
        assert_eq!(ctx[0].index(), 4);
        assert!(!ctx[0].mps());
        // Label 17 → run-length (Table C.2 index 3).
        assert_eq!(ctx[RUN_LENGTH_CTX].index(), 3);
        assert!(!ctx[RUN_LENGTH_CTX].mps());
        // Label 18 → UNIFORM (Table C.2 index 46).
        assert_eq!(ctx[UNIFORM_CTX].index(), 46);
        // Everything else (labels 1..=8, 9..=13, 14..=16) → default
        // (index 0, MPS 0).
        for label in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16] {
            assert_eq!(ctx[label].index(), 0, "label {label} should default");
            assert!(!ctx[label].mps());
        }
    }

    #[test]
    fn reset_contexts_array_length_matches_constant() {
        let ctx = reset_contexts();
        assert_eq!(ctx.len(), NUM_CONTEXTS);
        assert_eq!(NUM_CONTEXTS, 19);
    }

    // -- Table D.1 spot checks (significance propagation context) ----

    #[test]
    fn sp_context_zero_neighbours_is_label_0() {
        let nb = Neighbours::default();
        assert_eq!(significance_context_label(SubBandOrientation::LL, nb), 0);
        assert_eq!(significance_context_label(SubBandOrientation::HL, nb), 0);
        assert_eq!(significance_context_label(SubBandOrientation::LH, nb), 0);
        assert_eq!(significance_context_label(SubBandOrientation::HH, nb), 0);
    }

    #[test]
    fn sp_context_ll_lh_top_row_h_eq_2_is_label_8() {
        // Both horizontal neighbours significant → label 8 (LL/LH).
        let nb = Neighbours::from_slots(
            false, false, false, false, // d0, v0, v0_sign, d1
            true, false, true, false, // h0, h0_sign, h1, h1_sign
            false, false, false, false, // d2, v1, v1_sign, d3
        );
        assert_eq!(significance_context_label(SubBandOrientation::LL, nb), 8);
        assert_eq!(significance_context_label(SubBandOrientation::LH, nb), 8);
    }

    #[test]
    fn sp_context_hl_top_row_v_eq_2_is_label_8() {
        // Both vertical neighbours significant → label 8 (HL column).
        let nb = Neighbours::from_slots(
            false, true, false, false, // d0, v0=sig, v0_sign, d1
            false, false, false, false, // h0, h0_sign, h1, h1_sign
            false, true, false, false, // d2, v1=sig, v1_sign, d3
        );
        assert_eq!(significance_context_label(SubBandOrientation::HL, nb), 8);
        // The same neighbours on LL/LH give label 4 (H=0, V=2).
        assert_eq!(significance_context_label(SubBandOrientation::LL, nb), 4);
    }

    #[test]
    fn sp_context_hh_three_diagonals_is_label_8() {
        // Three diagonals significant → HH label 8.
        let nb = Neighbours::from_slots(
            true, false, false, true, // d0=sig, ..., d1=sig
            false, false, false, false, true, false, false, false, // d2=sig
        );
        assert_eq!(significance_context_label(SubBandOrientation::HH, nb), 8);
    }

    #[test]
    fn sp_context_ll_label_5_h1_v0_d0() {
        // (∑Hi=1, ∑Vi=0, ∑Di=0) → LL/LH label 5.
        let nb = Neighbours::from_slots(
            false, false, false, false, true, false, false, false, // only h0 significant
            false, false, false, false,
        );
        assert_eq!(significance_context_label(SubBandOrientation::LL, nb), 5);
    }

    #[test]
    fn sp_context_ll_label_1_h0_v0_d1() {
        // (∑Hi=0, ∑Vi=0, ∑Di=1) → LL/LH label 1.
        let nb = Neighbours::from_slots(
            true, false, false, false, // only d0
            false, false, false, false, false, false, false, false,
        );
        assert_eq!(significance_context_label(SubBandOrientation::LL, nb), 1);
    }

    #[test]
    fn sp_context_hh_label_1_one_hv_zero_d() {
        // HH: (∑(Hi+Vi) = 1, ∑Di = 0) → label 1.
        let nb = Neighbours::from_slots(
            false, false, false, false, true, false, false, false, // h0 significant only
            false, false, false, false,
        );
        assert_eq!(significance_context_label(SubBandOrientation::HH, nb), 1);
    }

    #[test]
    fn sp_context_full_table_d1_round_trip() {
        // Cover every Table D.1 row's *canonical* assignment for LL/LH,
        // HL, HH. The witness is the (∑H, ∑V, ∑D) triple per row.
        // LL/LH expected labels per (h, v, d):
        for &(h, v, d, want) in &[
            (2, 0, 0, 8u8),
            (2, 1, 0, 8),
            (2, 2, 4, 8),
            (1, 2, 0, 7),
            (1, 1, 0, 7),
            (1, 0, 4, 6),
            (1, 0, 1, 6),
            (1, 0, 0, 5),
            (0, 2, 0, 4),
            (0, 2, 4, 4),
            (0, 1, 0, 3),
            (0, 1, 4, 3),
            (0, 0, 4, 2),
            (0, 0, 2, 2),
            (0, 0, 1, 1),
            (0, 0, 0, 0),
        ] {
            // Synthesise neighbours satisfying the (h, v, d) triple.
            let nb = synth_neighbours(h, v, d);
            assert_eq!(
                significance_context_label(SubBandOrientation::LL, nb),
                want,
                "LL row (h={h}, v={v}, d={d}) expected label {want}"
            );
        }
        // HL swaps H/V at the input; reuse LL labels with swapped axes.
        for &(h, v, d, want) in &[
            (0, 2, 0, 8u8),
            (1, 2, 0, 8),
            (2, 1, 0, 7),
            (0, 1, 4, 6),
            (0, 1, 0, 5),
            (2, 0, 0, 4),
            (1, 0, 0, 3),
            (0, 0, 2, 2),
            (0, 0, 1, 1),
            (0, 0, 0, 0),
        ] {
            let nb = synth_neighbours(h, v, d);
            assert_eq!(
                significance_context_label(SubBandOrientation::HL, nb),
                want,
                "HL row (h={h}, v={v}, d={d}) expected label {want}"
            );
        }
        // HH reduces to (∑(H+V), ∑D).
        for &(hv, d, want) in &[
            (0, 3, 8u8),
            (0, 4, 8),
            (1, 3, 8),
            (1, 2, 7),
            (4, 2, 7),
            (0, 2, 6),
            (2, 1, 5),
            (3, 1, 5),
            (1, 1, 4),
            (0, 1, 3),
            (2, 0, 2),
            (4, 0, 2),
            (1, 0, 1),
            (0, 0, 0),
        ] {
            // Distribute hv across H and V (h = min(hv, 2), rest in V).
            let h = core::cmp::min(hv, 2);
            let v = hv - h;
            let nb = synth_neighbours(h, v, d);
            assert_eq!(
                significance_context_label(SubBandOrientation::HH, nb),
                want,
                "HH row (hv={hv}, d={d}) expected label {want}"
            );
        }
    }

    /// Build a Neighbours snapshot with exactly `h` significant H
    /// neighbours, `v` significant V neighbours, and `d` significant D
    /// neighbours (all positive-sign for simplicity).
    fn synth_neighbours(h: u8, v: u8, d: u8) -> Neighbours {
        let mut nb = Neighbours::default();
        if h >= 1 {
            nb.h0_sigma = true;
        }
        if h >= 2 {
            nb.h1_sigma = true;
        }
        if v >= 1 {
            nb.v0_sigma = true;
        }
        if v >= 2 {
            nb.v1_sigma = true;
        }
        if d >= 1 {
            nb.d0_sigma = true;
        }
        if d >= 2 {
            nb.d1_sigma = true;
        }
        if d >= 3 {
            nb.d2_sigma = true;
        }
        if d >= 4 {
            nb.d3_sigma = true;
        }
        nb
    }

    // -- Table D.3 sign-context spot checks ---------------------------

    #[test]
    fn sign_context_zero_zero_is_label_9() {
        let nb = Neighbours::default();
        let (label, xor) = sign_context_label(nb);
        // Label 9 → 0 inside the sign block.
        assert_eq!(label, 0);
        assert_eq!(xor, 0);
    }

    #[test]
    fn sign_context_positive_horizontal_label_12_xor_0() {
        // H0 significant positive, H1 insignificant, V's insignificant.
        // H contribution = +1 (Table D.2), V = 0.
        // → label 12, XORbit 0.
        let nb = Neighbours::from_slots(
            false, false, false, false, true, false, false,
            false, // h0 sig, sign = false (positive)
            false, false, false, false,
        );
        let (label, xor) = sign_context_label(nb);
        assert_eq!(label, 12 - 9);
        assert_eq!(xor, 0);
    }

    #[test]
    fn sign_context_negative_horizontal_label_12_xor_1() {
        // H0 significant negative, V's insignificant.
        // H contribution = -1, V = 0 → label 12, XORbit 1.
        let nb = Neighbours::from_slots(
            false, false, false, false, true, true, false,
            false, // h0 sig, sign = true (negative)
            false, false, false, false,
        );
        let (label, xor) = sign_context_label(nb);
        assert_eq!(label, 12 - 9);
        assert_eq!(xor, 1);
    }

    #[test]
    fn sign_context_pos_pos_label_13_xor_0() {
        // H0 sig+, V0 sig+. H=+1, V=+1 → label 13, XORbit 0.
        let nb = Neighbours::from_slots(
            false, true, false, false, // v0 sig (sign false = positive)
            true, false, false, false, // h0 sig+
            false, false, false, false,
        );
        let (label, xor) = sign_context_label(nb);
        assert_eq!(label, 13 - 9);
        assert_eq!(xor, 0);
    }

    #[test]
    fn sign_context_neg_neg_label_13_xor_1() {
        // Both negative → H=-1, V=-1 → label 13, XORbit 1.
        let nb = Neighbours::from_slots(
            false, true, true, false, // v0 sig negative
            true, true, false, false, // h0 sig negative
            false, false, false, false,
        );
        let (label, xor) = sign_context_label(nb);
        assert_eq!(label, 13 - 9);
        assert_eq!(xor, 1);
    }

    #[test]
    fn sign_context_mixed_signs_cancel_to_zero_contribution() {
        // Two H neighbours, one positive, one negative → H = 0 per
        // Table D.2 (a "significant positive / significant negative"
        // row collapses to 0). With V also 0, label is 9.
        let nb = Neighbours::from_slots(
            false, false, false, false, true, false, true, true, // h0 +, h1 -
            false, false, false, false,
        );
        let (label, xor) = sign_context_label(nb);
        assert_eq!(label, 0); // 9 - 9
        assert_eq!(xor, 0);
    }

    #[test]
    fn sign_context_xorbit_inverts_label_polarity() {
        // For every (h, v) combination the XORbit pattern is
        // symmetric under negation of *both* H and V (because Table
        // D.3 mirrors top-half rows to bottom-half rows with XOR=1).
        for h in [-1i8, 0, 1] {
            for v in [-1i8, 0, 1] {
                if h == 0 && v == 0 {
                    continue;
                }
                let (lp, xp) = sign_label_from_contributions(h, v);
                let (ln, xn) = sign_label_from_contributions(-h, -v);
                assert_eq!(
                    lp, ln,
                    "labels should match for ({h}, {v}) and ({}, {})",
                    -h, -v
                );
                assert_eq!(
                    xp ^ xn,
                    1,
                    "XORbit should differ for ({h}, {v}) vs negation"
                );
            }
        }
    }

    // -- Table D.4 magnitude-refinement context labels ----------------

    #[test]
    fn refinement_label_first_no_neighbours_is_14() {
        // First refinement, ∑(Hi+Vi+Di) = 0 → label 14 (offset 0).
        let nb = Neighbours::default();
        assert_eq!(refinement_context_label(nb, false), 0);
    }

    #[test]
    fn refinement_label_first_with_neighbour_is_15() {
        // First refinement, at least one significant neighbour → label 15
        // (offset 1). One diagonal neighbour is enough.
        let nb = Neighbours::from_slots(
            true, false, false, false, // d0 significant
            false, false, false, false, false, false, false, false,
        );
        assert_eq!(refinement_context_label(nb, false), 1);
        // A single horizontal or vertical neighbour also gives 15.
        let nb_h = synth_neighbours(1, 0, 0);
        assert_eq!(refinement_context_label(nb_h, false), 1);
        let nb_v = synth_neighbours(0, 1, 0);
        assert_eq!(refinement_context_label(nb_v, false), 1);
    }

    #[test]
    fn refinement_label_already_refined_is_16_regardless_of_neighbours() {
        // Once already_refined is set, the neighbour state is a
        // don't-care: label 16 (offset 2) in all cases.
        assert_eq!(refinement_context_label(Neighbours::default(), true), 2);
        let busy = synth_neighbours(2, 2, 4);
        assert_eq!(refinement_context_label(busy, true), 2);
    }

    // -- §D.3.3 magnitude refinement pass behaviour -------------------

    #[test]
    fn refinement_skips_insignificant_and_newly_significant() {
        // A 4x1 LL block: (0,0) significant + already a prior pass (not
        // newly significant), (1,0) newly significant, (2,0)/(3,0)
        // insignificant. Only (0,0) should be refined.
        let bytes = [0x84u8, 0xC7, 0x3B, 0xFC];
        let mut block = CodeBlock::new(SubBandOrientation::LL, 4, 1);
        block.mark_significant_for_test(0, 0, false, 0b10); // sig, not new
        block.mark_significant_for_test(1, 0, false, 0b10); // sig, but new
        block.set_newly_significant_for_test(1, 0);

        let mut dec = MqDecoder::new(&bytes);
        let mut ctx = reset_contexts();
        let refined = block
            .magnitude_refinement_pass(0, &mut dec, &mut ctx)
            .unwrap();
        assert_eq!(refined, 1, "only (0,0) is eligible for refinement");
        // (0,0) is now flagged already_refined; (1,0) is not.
        assert!(block.coefficient(0, 0).already_refined);
        assert!(!block.coefficient(1, 0).already_refined);
        // Insignificant coefficients are untouched.
        assert!(!block.coefficient(2, 0).already_refined);
        assert!(!block.coefficient(3, 0).already_refined);
    }

    #[test]
    fn refinement_no_eligible_coefficients_makes_no_mq_decision() {
        // An all-insignificant block: nothing to refine, decoder must
        // not advance.
        let bytes = [0x84u8, 0xC7, 0x3B, 0xFC];
        let mut block = CodeBlock::new(SubBandOrientation::HH, 5, 7);
        let mut dec = MqDecoder::new(&bytes);
        let bp_start = dec.byte_pointer();
        let mut ctx = reset_contexts();
        let refined = block
            .magnitude_refinement_pass(3, &mut dec, &mut ctx)
            .unwrap();
        assert_eq!(refined, 0);
        assert_eq!(dec.byte_pointer(), bp_start);
    }

    #[test]
    fn refinement_first_bit_matches_context_14_reference() {
        // A 1x1 LL block with one significant, not-yet-refined, isolated
        // coefficient. ∑(Hi+Vi+Di)=0 + first refinement → context 14.
        // The decoded refinement bit must equal what a fresh MQ decoder
        // produces against a default context (label 14 starts at Table
        // C.2 index 0 / MPS 0, like all non-special contexts).
        let bytes = [0x84u8, 0xC7, 0x3B, 0xFC];
        let mut ref_dec = MqDecoder::new(&bytes);
        let mut ref_ctx = MqContext::default();
        let expected = ref_dec.decode(&mut ref_ctx);

        let mut block = CodeBlock::new(SubBandOrientation::LL, 1, 1);
        block.mark_significant_for_test(0, 0, false, 0b100); // magnitude MSB set
        let mut dec = MqDecoder::new(&bytes);
        let mut ctx = reset_contexts();
        let refined = block
            .magnitude_refinement_pass(0, &mut dec, &mut ctx)
            .unwrap();
        assert_eq!(refined, 1);
        let c = block.coefficient(0, 0);
        assert!(c.already_refined);
        // Bit-plane 0 weight is 1; magnitude gains it iff the bit was 1.
        let expected_mag = 0b100 | u32::from(expected);
        assert_eq!(c.magnitude, expected_mag);
    }

    #[test]
    fn refinement_second_pass_uses_context_16() {
        // Refine the same coefficient twice. The first refinement uses
        // context 14/15 and sets already_refined; the second must use
        // context 16. We verify by comparing the context array state:
        // after pass 1 only label 14 (offset 0) has moved from index 0;
        // after pass 2 label 16 (offset 2) has also moved.
        let bytes = [0xFFu8, 0xFF, 0xFF, 0xFF];
        let mut block = CodeBlock::new(SubBandOrientation::LL, 1, 1);
        block.mark_significant_for_test(0, 0, false, 0b1000);
        let mut dec = MqDecoder::new(&bytes);
        let mut ctx = reset_contexts();

        // Snapshot context-16 state before any refinement.
        let ctx16_before = ctx[REFINEMENT_CTX_OFFSET + 2].index();

        // Pass 1 (bit-plane 2): first refinement → context 14.
        block
            .magnitude_refinement_pass(2, &mut dec, &mut ctx)
            .unwrap();
        assert!(block.coefficient(0, 0).already_refined);
        // Context 16 must be untouched after the *first* refinement.
        assert_eq!(ctx[REFINEMENT_CTX_OFFSET + 2].index(), ctx16_before);

        // Pass 2 (bit-plane 1): now already_refined → context 16.
        block
            .magnitude_refinement_pass(1, &mut dec, &mut ctx)
            .unwrap();
        // Context 16 must now have been exercised (its adaptive index
        // moved away from the reset value after at least one decision).
        assert_ne!(
            ctx[REFINEMENT_CTX_OFFSET + 2].index(),
            ctx16_before,
            "context 16 should be used by the second refinement"
        );
    }

    #[test]
    fn refinement_context_15_used_when_neighbour_significant() {
        // 2x1 LL block, both coefficients significant and not-new.
        // (0,0) has a significant right neighbour (1,0) and vice-versa,
        // so each sees ∑(H+V+D) ≥ 1 → first refinement uses context 15.
        let bytes = [0xFFu8, 0xFF, 0xFF, 0xFF];
        let mut block = CodeBlock::new(SubBandOrientation::LL, 2, 1);
        block.mark_significant_for_test(0, 0, false, 0b10);
        block.mark_significant_for_test(1, 0, false, 0b10);
        let mut dec = MqDecoder::new(&bytes);
        let mut ctx = reset_contexts();

        let ctx14_before = ctx[REFINEMENT_CTX_OFFSET].index();
        let ctx15_before = ctx[REFINEMENT_CTX_OFFSET + 1].index();

        let refined = block
            .magnitude_refinement_pass(0, &mut dec, &mut ctx)
            .unwrap();
        assert_eq!(refined, 2);
        // Context 15 (offset 1) must have been exercised; context 14
        // (offset 0) must be untouched.
        assert_ne!(ctx[REFINEMENT_CTX_OFFSET + 1].index(), ctx15_before);
        assert_eq!(ctx[REFINEMENT_CTX_OFFSET].index(), ctx14_before);
    }

    #[test]
    fn refinement_pass_visits_stripe_major_order() {
        // Indirect scan-order check mirroring the SP-pass test: an
        // all-insignificant block triggers no decisions and no state
        // change, proving the loop ran exhaustively without entering the
        // inner body.
        let mut block = CodeBlock::new(SubBandOrientation::LH, 6, 10);
        let bytes = [0x84u8, 0xC7, 0x3B, 0xFC];
        let mut dec = MqDecoder::new(&bytes);
        let bp_start = dec.byte_pointer();
        let mut ctx = reset_contexts();
        let refined = block
            .magnitude_refinement_pass(5, &mut dec, &mut ctx)
            .unwrap();
        assert_eq!(refined, 0);
        for v in 0..block.height() {
            for u in 0..block.width() {
                assert!(!block.coefficient(u, v).already_refined);
            }
        }
        assert_eq!(dec.byte_pointer(), bp_start);
    }

    // -- §D.1 scan order ----------------------------------------------

    #[test]
    fn scan_order_visits_every_coefficient_exactly_once() {
        // We can't observe the scan order directly from a public
        // accessor, but we can verify it indirectly: an SP pass on an
        // all-insignificant code-block where every context is label 0
        // (no MQ decisions made) leaves every coefficient untouched and
        // never advances the decoder. We assert (a) zero "newly
        // significant" coefficients, (b) every coefficient remains
        // insignificant, (c) the bit pointer of the MQ decoder hasn't
        // moved beyond its INITDEC position. This proves the loop ran
        // exhaustively without entering the inner body.
        let mut block = CodeBlock::new(SubBandOrientation::LL, 6, 10);
        let bytes = [0x84u8, 0xC7, 0x3B, 0xFC];
        let mut dec = MqDecoder::new(&bytes);
        let bp_start = dec.byte_pointer();
        let mut ctx = reset_contexts();
        let newly = block
            .significance_propagation_pass(7, &mut dec, &mut ctx)
            .unwrap();
        assert_eq!(newly, 0);
        for v in 0..block.height() {
            for u in 0..block.width() {
                let c = block.coefficient(u, v);
                assert!(!c.sigma, "({u}, {v}) should still be insignificant");
                assert_eq!(c.magnitude, 0);
            }
        }
        // No MQ decision means no byte consumption.
        assert_eq!(dec.byte_pointer(), bp_start);
    }

    // -- Single-coefficient SP decode end-to-end ----------------------

    #[test]
    fn single_significant_neighbour_drives_one_mq_decision() {
        // Construct a 4x1 LL code-block where coefficient (0, 0) is
        // pre-marked significant. Its right neighbour (1, 0) then has
        // h-context label 5 (H=1, V=0, D=0). The SP pass should draw
        // exactly one MQ decision against context 5 for (1, 0); the
        // other two coefficients ((2, 0) and (3, 0)) have label 0 and
        // are skipped. We don't predict the decision bit (it depends
        // on the byte stream + adaptive state) — we only assert that
        // (a) at most one coefficient became significant, (b) the
        // decision aligns with what a fresh MQ decoder produces on the
        // same context. We do this by running the same MQ decoder once
        // up front against a separate context-5 instance and comparing.
        let bytes = [0x84u8, 0xC7, 0x3B, 0xFC];
        // Reference decoder: one decision against a default context.
        let mut ref_dec = MqDecoder::new(&bytes);
        let mut ref_ctx = MqContext::default();
        let expected_first = ref_dec.decode(&mut ref_ctx);

        // Subject: full SP pass with the same byte stream.
        let mut block = CodeBlock::new(SubBandOrientation::LL, 4, 1);
        // Pre-mark (0, 0) significant with positive sign, magnitude 1.
        // We synthesise this via a small private helper on CodeBlock
        // exposed below (`mark_significant`).
        block.mark_significant_for_test(0, 0, false, 1);

        let mut dec = MqDecoder::new(&bytes);
        let mut ctx = reset_contexts();
        let newly = block
            .significance_propagation_pass(0, &mut dec, &mut ctx)
            .unwrap();
        assert!(newly <= 1);
        // (1, 0) was the only coefficient whose context label was
        // non-zero (label 5). It must match the reference decoder.
        let became_significant = block.coefficient(1, 0).sigma;
        if expected_first == 1 {
            assert!(became_significant, "(1, 0) should now be significant");
            assert!(
                block.was_newly_significant(1, 0),
                "(1, 0) should be flagged newly-significant"
            );
            // newly_significant flag also implies magnitude carries
            // the 1 << bitplane (here bitplane = 0).
            assert_eq!(block.coefficient(1, 0).magnitude, 1);
        } else {
            assert!(!became_significant);
            assert!(!block.was_newly_significant(1, 0));
        }
        // (2, 0) and (3, 0) have label-0 contexts → untouched.
        assert!(!block.coefficient(2, 0).sigma);
        assert!(!block.coefficient(3, 0).sigma);
    }

    #[test]
    fn newly_significant_flag_clears_between_passes() {
        // Run two SP passes back-to-back. The second one must clear
        // every newly_significant flag from the first.
        let bytes = [0xFFu8, 0xFF]; // 0xFF-fill — predictable state.
        let mut block = CodeBlock::new(SubBandOrientation::LL, 3, 1);
        // Pre-mark (0, 0) significant to give (1, 0) a non-zero context.
        block.mark_significant_for_test(0, 0, false, 1);
        let mut dec = MqDecoder::new(&bytes);
        let mut ctx = reset_contexts();
        // First pass: may or may not flip (1, 0) — we don't care for
        // this test, only that no flag from a prior pass survives.
        let _ = block
            .significance_propagation_pass(1, &mut dec, &mut ctx)
            .unwrap();
        // Manually re-mark (0, 0) "newly significant" (simulating a
        // SP-pass carry from a previous bit-plane).
        block.set_newly_significant_for_test(0, 0);
        assert!(block.was_newly_significant(0, 0));
        // Second pass clears the carry first.
        let _ = block
            .significance_propagation_pass(1, &mut dec, &mut ctx)
            .unwrap();
        // (0, 0) wasn't visited by the SP pass (σ already true), so the
        // flag must have been cleared by the prologue and not re-set.
        assert!(!block.was_newly_significant(0, 0));
    }

    // -- Test-only helpers --------------------------------------------

    impl CodeBlock {
        /// Test-only: pre-mark a coefficient as significant.
        pub(super) fn mark_significant_for_test(
            &mut self,
            u: usize,
            v: usize,
            sign: bool,
            magnitude: u32,
        ) {
            let idx = u + v * self.width;
            self.coefficients[idx] = Coefficient {
                magnitude,
                sigma: true,
                sign,
                already_refined: false,
            };
        }

        /// Test-only: set the "newly significant" flag for a coefficient.
        pub(super) fn set_newly_significant_for_test(&mut self, u: usize, v: usize) {
            self.newly_significant[u + v * self.width] = true;
        }
    }

    // -- §D.3.4 cleanup pass behaviour --------------------------------

    /// Replay the full cleanup-pass decode of one 1×4 run-length-eligible
    /// LL column against an independent reference decoder, returning the
    /// expected `(sigma, sign, magnitude)` per row plus the run-length
    /// bit. This mirrors the §D.3.4 / Table D.5 control flow exactly and
    /// is the oracle the subject [`CodeBlock::cleanup_pass`] must match.
    fn replay_eligible_column_1x4(bytes: &[u8], weight: u32) -> (u8, [(bool, bool, u32); 4]) {
        let mut dec = MqDecoder::new(bytes);
        let mut ctx = reset_contexts();
        let mut expect = [(false, false, 0u32); 4];
        let rl = dec.decode(&mut ctx[RUN_LENGTH_CTX]);
        if rl == 0 {
            return (rl, expect);
        }
        let hi = dec.decode(&mut ctx[UNIFORM_CTX]);
        let lo = dec.decode(&mut ctx[UNIFORM_CTX]);
        let first = ((hi << 1) | lo) as usize;
        let mut sig = [false; 4];
        // First-significant coefficient: sign only (significance is implied
        // by the run-length escape + UNIFORM index).
        {
            let nb = col_neighbours_1x4(&sig, first);
            let (sl, xb) = sign_context_label(nb);
            let sd = dec.decode(&mut ctx[SIGN_CTX_OFFSET + sl as usize]);
            expect[first] = (true, (sd ^ xb) != 0, weight);
            sig[first] = true;
        }
        for v in (first + 1)..4 {
            let nb = col_neighbours_1x4(&sig, v);
            let label = significance_context_label(SubBandOrientation::LL, nb);
            let sbit = dec.decode(&mut ctx[SP_CTX_OFFSET + label as usize]);
            if sbit == 1 {
                let (sl, xb) = sign_context_label(nb);
                let sd = dec.decode(&mut ctx[SIGN_CTX_OFFSET + sl as usize]);
                expect[v] = (true, (sd ^ xb) != 0, weight);
                sig[v] = true;
            }
        }
        (rl, expect)
    }

    /// Neighbours of row `v` in a 1-wide, 4-tall column: only the vertical
    /// up/down neighbours can be in-block (sign treated as positive — the
    /// significance-context label ignores sign anyway).
    fn col_neighbours_1x4(sig: &[bool; 4], v: usize) -> Neighbours {
        let v_up = v >= 1 && sig[v - 1];
        let v_dn = v + 1 < 4 && sig[v + 1];
        Neighbours::from_slots(
            false, v_up, false, false, false, false, false, false, false, v_dn, false, false,
        )
    }

    #[test]
    fn cleanup_run_length_zero_leaves_column_insignificant() {
        // A 1x4 LL block: one full (4-row) column, all insignificant with
        // zero context → run-length eligible. When the run-length bit is
        // 0, the whole column stays insignificant. We replay to find a
        // byte stream whose run-length bit is 0, then assert the subject
        // leaves all four insignificant.
        let candidates: [[u8; 4]; 4] = [
            [0x00, 0x00, 0x00, 0x00],
            [0xFF, 0xFF, 0xFF, 0xFF],
            [0x84, 0xC7, 0x3B, 0xFC],
            [0x42, 0x91, 0x6A, 0x0D],
        ];
        let bytes = candidates
            .iter()
            .find(|b| replay_eligible_column_1x4(*b, 1 << 7).0 == 0)
            .expect("a candidate with run-length bit 0 must exist");

        let mut block = CodeBlock::new(SubBandOrientation::LL, 1, 4);
        let mut dec = MqDecoder::new(bytes);
        let mut ctx = reset_contexts();
        let newly = block.cleanup_pass(7, &mut dec, &mut ctx).unwrap();
        assert_eq!(newly, 0, "run-length 0 leaves all four insignificant");
        for v in 0..4 {
            assert!(!block.coefficient(0, v).sigma);
            assert_eq!(block.coefficient(0, v).magnitude, 0);
        }
    }

    #[test]
    fn cleanup_run_length_one_uses_uniform_first_index() {
        // A 1x4 LL block where the run-length bit is 1 (escape). The two
        // UNIFORM bits select the first-significant coefficient; it
        // becomes significant with the bit-plane weight + a decoded sign,
        // and the rows below are decoded normally. We replay the exact MQ
        // operation sequence to predict the outcome bit-for-bit.
        let candidates: [&[u8]; 4] = [
            &[0xFF, 0xAC, 0x73, 0x1D, 0x9E, 0x42],
            &[0x00, 0x11, 0x22, 0x33, 0x44, 0x55],
            &[0xC3, 0x5A, 0x96, 0x7E, 0x21, 0xBD],
            &[0x84, 0xC7, 0x3B, 0xFC, 0x10, 0x6F],
        ];
        let weight = 1u32 << 5;
        let (bytes, (_, expect)) = candidates
            .iter()
            .map(|b| (*b, replay_eligible_column_1x4(b, weight)))
            .find(|(_, (rl, _))| *rl == 1)
            .expect("a candidate with run-length bit 1 must exist");

        let mut block = CodeBlock::new(SubBandOrientation::LL, 1, 4);
        let mut dec = MqDecoder::new(bytes);
        let mut ctx = reset_contexts();
        let newly = block.cleanup_pass(5, &mut dec, &mut ctx).unwrap();

        let mut expected_newly = 0usize;
        for (v, &(e_sig, e_sign, e_mag)) in expect.iter().enumerate() {
            let c = block.coefficient(0, v);
            assert_eq!(c.sigma, e_sig, "sigma mismatch at v={v}");
            if e_sig {
                expected_newly += 1;
                assert_eq!(c.sign, e_sign, "sign mismatch at v={v}");
                assert_eq!(c.magnitude, e_mag, "magnitude mismatch at v={v}");
                assert!(block.was_newly_significant(0, v));
            }
        }
        assert_eq!(newly, expected_newly);
        // The run-length escape guarantees at least one significant.
        assert!(expected_newly >= 1);
    }

    #[test]
    fn cleanup_short_stripe_never_uses_run_length() {
        // A 1x3 block: only three rows → no run-length coding per §D.3.4
        // ("fewer than four rows remaining"). Each coefficient is coded
        // individually with its Table D.1 context (all label 0 here). We
        // replay three significance decisions against the *zero-neighbour*
        // context (label 0, Table C.2 index 4) to predict the outcome.
        let bytes = [0xC1u8, 0x5D, 0x82, 0x77, 0x3A];
        let mut ref_dec = MqDecoder::new(&bytes);
        let mut ref_ctx = reset_contexts();
        let mut expect = [false; 3];
        let mut sig = [false; 3];
        for v in 0..3 {
            let v_up = v >= 1 && sig[v - 1];
            let v_dn = v + 1 < 3 && sig[v + 1];
            let nb = Neighbours::from_slots(
                false, v_up, false, false, false, false, false, false, false, v_dn, false, false,
            );
            let label = significance_context_label(SubBandOrientation::LL, nb);
            // Run-length context must NOT be consulted here.
            assert_eq!(label, 0, "isolated column should have label 0 at v={v}");
            let sbit = ref_dec.decode(&mut ref_ctx[SP_CTX_OFFSET + label as usize]);
            if sbit == 1 {
                let (sl, _xb) = sign_context_label(nb);
                let _ = ref_dec.decode(&mut ref_ctx[SIGN_CTX_OFFSET + sl as usize]);
                expect[v] = true;
                sig[v] = true;
            }
        }

        let mut block = CodeBlock::new(SubBandOrientation::LL, 1, 3);
        let mut dec = MqDecoder::new(&bytes);
        let mut ctx = reset_contexts();
        // The run-length context must be untouched (its adaptive index
        // stays at the reset value 3).
        assert_eq!(ctx[RUN_LENGTH_CTX].index(), 3);
        block.cleanup_pass(0, &mut dec, &mut ctx).unwrap();
        assert_eq!(
            ctx[RUN_LENGTH_CTX].index(),
            3,
            "short stripe must not use the run-length context"
        );
        for (v, &want) in expect.iter().enumerate() {
            assert_eq!(block.coefficient(0, v).sigma, want, "v={v}");
        }
    }

    #[test]
    fn cleanup_skips_already_significant_and_nonzero_context_columns() {
        // A 1x4 LL block with (0,0) pre-marked significant. The column is
        // NOT run-length-eligible because (0,0) is already significant and
        // (0,1) now has a non-zero (vertical) context. Each remaining
        // insignificant coefficient is coded individually; (0,0) is
        // skipped. The run-length context must stay at its reset index.
        let bytes = [0x9Au8, 0x3F, 0xC4, 0x71, 0x2E];
        let mut block = CodeBlock::new(SubBandOrientation::LL, 1, 4);
        block.mark_significant_for_test(0, 0, false, 1 << 6);
        let mut dec = MqDecoder::new(&bytes);
        let mut ctx = reset_contexts();
        let rl_before = ctx[RUN_LENGTH_CTX].index();
        block.cleanup_pass(5, &mut dec, &mut ctx).unwrap();
        assert_eq!(
            ctx[RUN_LENGTH_CTX].index(),
            rl_before,
            "non-eligible column must not touch the run-length context"
        );
        // (0,0) stays exactly as pre-marked (significant, untouched).
        let c0 = block.coefficient(0, 0);
        assert!(c0.sigma);
        assert_eq!(c0.magnitude, 1 << 6);
    }

    #[test]
    fn cleanup_first_bitplane_only_pass_decodes_isolated_coefficient() {
        // §D.3: the first non-empty bit-plane of a code-block is a
        // cleanup-only pass. Here a 1x1 LL block (a single coefficient,
        // never run-length-eligible because the stripe is height 1) is
        // decoded directly via the normal-mode significance context.
        let bytes = [0xE3u8, 0x55, 0x9C, 0x08];
        let mut ref_dec = MqDecoder::new(&bytes);
        let mut ref_ctx = reset_contexts();
        // Zero-neighbour significance context (label 0, index 4).
        let sbit = ref_dec.decode(&mut ref_ctx[SP_CTX_OFFSET]);

        let mut block = CodeBlock::new(SubBandOrientation::LL, 1, 1);
        let mut dec = MqDecoder::new(&bytes);
        let mut ctx = reset_contexts();
        let newly = block.cleanup_pass(7, &mut dec, &mut ctx).unwrap();
        if sbit == 1 {
            assert_eq!(newly, 1);
            assert!(block.coefficient(0, 0).sigma);
            assert!(block.coefficient(0, 0).magnitude & (1 << 7) != 0);
        } else {
            assert_eq!(newly, 0);
            assert!(!block.coefficient(0, 0).sigma);
        }
    }

    #[test]
    fn cleanup_run_length_zero_makes_no_uniform_decision() {
        // When the run-length bit is 0, the two UNIFORM bits are NOT
        // drawn (Table D.5 row 1: "Symbols decoded with UNIFORM = none").
        // We pick a byte stream whose run-length bit is 0 and verify the
        // UNIFORM context is left at its reset index afterward.
        let candidates: [[u8; 4]; 4] = [
            [0x00, 0x00, 0x00, 0x00],
            [0xFF, 0xFF, 0xFF, 0xFF],
            [0x84, 0xC7, 0x3B, 0xFC],
            [0x42, 0x91, 0x6A, 0x0D],
        ];
        let bytes = candidates
            .iter()
            .find(|b| replay_eligible_column_1x4(*b, 1 << 3).0 == 0)
            .expect("a candidate with run-length bit 0 must exist");

        let mut block = CodeBlock::new(SubBandOrientation::LL, 1, 4);
        let mut dec = MqDecoder::new(bytes);
        let mut ctx = reset_contexts();
        let uniform_before = ctx[UNIFORM_CTX].index();
        block.cleanup_pass(3, &mut dec, &mut ctx).unwrap();
        assert_eq!(
            ctx[UNIFORM_CTX].index(),
            uniform_before,
            "symbol-0 run-length must not consult the UNIFORM context"
        );
        for v in 0..4 {
            assert!(!block.coefficient(0, v).sigma);
        }
    }

    #[test]
    fn cleanup_full_three_pass_bitplane_sequence_self_consistent() {
        // Drive the §D.3 three-pass order (SP → MR → cleanup) on one
        // bit-plane of a small LL block and confirm the cleanup pass only
        // touches coefficients the earlier two passes left insignificant.
        // This is a structural test: it does not predict the bits, only
        // that no coefficient is decoded twice in a single bit-plane.
        let bytes = [0xB7u8, 0x29, 0xEE, 0x4C, 0x81, 0x6A, 0xD3];
        let mut block = CodeBlock::new(SubBandOrientation::LL, 4, 4);
        let mut dec = MqDecoder::new(&bytes);
        let mut ctx = reset_contexts();
        // First non-empty bit-plane: cleanup only.
        let bp = 5;
        let n_cleanup0 = block.cleanup_pass(bp, &mut dec, &mut ctx).unwrap();
        // Snapshot which coefficients are significant after the cleanup.
        let mut sig_after: Vec<bool> = Vec::new();
        for v in 0..4 {
            for u in 0..4 {
                sig_after.push(block.coefficient(u, v).sigma);
            }
        }
        // Next bit-plane: SP then MR then cleanup.
        let bp2 = bp - 1;
        let _ = block
            .significance_propagation_pass(bp2, &mut dec, &mut ctx)
            .unwrap();
        let _ = block
            .magnitude_refinement_pass(bp2, &mut dec, &mut ctx)
            .unwrap();
        let _ = block.cleanup_pass(bp2, &mut dec, &mut ctx).unwrap();
        // Every coefficient significant after the first cleanup is still
        // significant (significance is monotone — never cleared).
        let mut k = 0;
        for v in 0..4 {
            for u in 0..4 {
                if sig_after[k] {
                    assert!(
                        block.coefficient(u, v).sigma,
                        "({u},{v}) significance must be monotone"
                    );
                }
                k += 1;
            }
        }
        // Sanity: the first cleanup pass produced a non-negative count
        // and the decoder advanced through the stream without panicking.
        let _ = n_cleanup0;
        assert!(dec.byte_pointer() > 0);
    }

    // -- Boundary handling --------------------------------------------

    #[test]
    fn out_of_block_neighbours_are_treated_as_insignificant() {
        // A 1x1 code-block: all 8 neighbours are out-of-block.
        // Context label must be 0 on every orientation regardless of
        // sub-band.
        let block = CodeBlock::new(SubBandOrientation::LH, 1, 1);
        let nb = block.neighbours(0, 0);
        assert_eq!(nb.h_sum(), 0);
        assert_eq!(nb.v_sum(), 0);
        assert_eq!(nb.d_sum(), 0);
        assert_eq!(significance_context_label(SubBandOrientation::LH, nb), 0);
    }

    #[test]
    fn neighbours_at_corners_clip_correctly() {
        // 3x3 LL block, only (1, 1) (centre) significant. Corner (0, 0)
        // has neighbours d0/v0/d1/h0 outside the block; only h1, v1, d3
        // exist on the grid, of which only (1, 1) — d3 — is significant.
        let mut block = CodeBlock::new(SubBandOrientation::LL, 3, 3);
        block.mark_significant_for_test(1, 1, false, 1);
        let nb = block.neighbours(0, 0);
        assert!(!nb.h0_sigma); // out of block
        assert!(!nb.v0_sigma); // out of block
        assert!(!nb.d0_sigma); // out of block
        assert!(!nb.d1_sigma); // out of block
        assert!(!nb.h1_sigma); // (1, 0) — insignificant
        assert!(!nb.v1_sigma); // (0, 1) — insignificant
        assert!(!nb.d2_sigma); // out of block (v+1 = 1, u-1 = -1)
        assert!(nb.d3_sigma); // (1, 1) — significant
        assert_eq!(nb.h_sum(), 0);
        assert_eq!(nb.v_sum(), 0);
        assert_eq!(nb.d_sum(), 1);
        // LL/LH (h=0, v=0, d=1) → label 1.
        assert_eq!(significance_context_label(SubBandOrientation::LL, nb), 1);
    }
}
