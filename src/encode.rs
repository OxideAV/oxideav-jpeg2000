//! J2K codestream **encoder** — lossless (reversible 5-3) Part-1 path.
//!
//! Composes the encode-side subsystems built across this crate into a
//! working end-to-end encoder:
//!
//! * the §G.1.2 forward DC level shift,
//! * the §F.4 forward 5-3 DWT cascade ([`crate::dwt::sd_2d_5x3`], one
//!   analysis per decomposition level, recursing on the LL band),
//! * the Annex D tier-1 forward coding passes
//!   ([`crate::t1::CodeBlock`]'s `*_encode` methods driving
//!   [`crate::mqenc::MqEncoder`]) — the full §D.3 schedule (cleanup on
//!   the first non-empty bit-plane, then SP → MR → cleanup triples down
//!   to plane 0) coding every bit-plane into one §C.3 codeword segment,
//! * the §B.10 tier-2 packet-header writer
//!   ([`crate::packet::encode_packet_header`]), and
//! * the Annex A marker writers (`SOC`, `SIZ`, `COD`, `QCD`, `SOT`,
//!   `SOD`, `EOC`) assembled in the §A.3 codestream order.
//!
//! The produced codestream is a Part-1 J2K stream: single tile at the
//! reference-grid origin, one quality layer, any of the five §B.12.1
//! progression orders ([`EncodeParams::progression`]), maximum
//! precincts (`PPx = PPy = 15`), optional Table A.17 MCT, and either
//! the reversible 5-3 kernel with the "no quantization" style (T.800
//! Table A.28 style 0 — fully **lossless**: decoding reproduces the
//! input samples bit-exactly) or the irreversible 9-7 kernel with
//! Annex E scalar-expounded quantisation.
//!
//! The geometry (tile / resolution-level / precinct / code-block
//! enumeration) is obtained from the same [`crate::geometry`] functions
//! the decoder uses, so the encoder's packet layout agrees with the
//! decoder's walk by construction; packets are emitted in the order the
//! [`crate::progression`] drivers enumerate them.
//!
//! ## Quantisation exponents
//!
//! For the reversible transform the `SPqcd` exponent for a sub-band `b`
//! is `εb = RI + gain_b` (the Table E.1 log-gain: 0 for LL, 1 for
//! HL / LH, 2 for HH), with `G = 2` guard bits, so the Equation E-2 bit
//! budget is `Mb = G + εb − 1`. Each code-block's number of coded
//! bit-planes is its actual magnitude width; the §B.10.5 zero-bit-plane
//! count is `P = Mb − planes` and the pass count is the full-schedule
//! `3 · planes − 2`, so the decoder's §D.3 cap holds with equality.
//!
//! ## Clean-room provenance
//!
//! Written solely from the T.800 documents under
//! `docs/image/jpeg2000/`: Annex A marker syntax (Tables A.9 / A.13 –
//! A.21 / A.27 – A.28 and §A.4.2 / §A.6.1 / §A.6.4), Annex B geometry
//! and packet formation, Annex C / D entropy coding, §F.4 forward
//! transform, §G.1.2 level shift, Annex E quantisation. Validation is
//! a round-trip through this crate's own independently-written decoder.

use crate::dwt::{sd_2d_5x3, Interleaved2D};
use crate::geometry::{
    derive_precinct_code_blocks, derive_precinct_partition, derive_resolution_levels,
    derive_tile_geometry, precinct_exponents_at, SubBandOrientation,
};
use crate::mqenc::MqEncoder;
use crate::packet::{
    encode_packet_header, CodeBlockPlan, PrecinctEncoderState, SubBandEncoderPlan, SubBandGeometry,
};
use crate::progression::{
    cprl_packet_order, lrcp_packet_order, pcrl_packet_order, rlcp_packet_order, rpcl_packet_order,
    ComponentPositionInfo, ComponentProgressionInfo, ResolutionPrecinctLayout,
};
use crate::t1::{reset_contexts, CodeBlock, Coefficient};
use crate::{
    Error, ProgressionOrder, Siz, SizComponent, MARKER_COD, MARKER_EOC, MARKER_QCD, MARKER_SIZ,
    MARKER_SOC, MARKER_SOD, MARKER_SOT,
};

/// Guard-bit count `G` signalled in `Sqcd` (Table A.28) and used in the
/// Equation E-2 budget `Mb = G + εb − 1`.
const GUARD_BITS: u8 = 2;

/// Table E.1 log2 gain of the reversible 5-3 sub-bands.
fn band_gain(orientation: SubBandOrientation) -> u8 {
    match orientation {
        SubBandOrientation::LL => 0,
        SubBandOrientation::HL | SubBandOrientation::LH => 1,
        SubBandOrientation::HH => 2,
    }
}

/// One sub-band's coefficient plane (absolute band coordinates start at
/// zero for an origin-anchored image, so `data[u + v * width]` is band
/// sample `(u, v)`).
struct BandPlane {
    width: usize,
    height: usize,
    data: Vec<i32>,
}

/// The full forward-DWT decomposition of one component: `ll` is the
/// final `NL`-level LL band; `high[l]` holds the `(HL, LH, HH)` planes
/// of decomposition level `l + 1` (1-based level `1..=NL`, level `NL`
/// deepest).
struct ComponentBands {
    ll: BandPlane,
    high: Vec<[BandPlane; 3]>,
}

/// §F.3.3 deinterleave: split one analysis output lattice into its four
/// sub-band planes (`LL` at `(2u, 2v)`, `HL` at `(2u+1, 2v)`, `LH` at
/// `(2u, 2v+1)`, `HH` at `(2u+1, 2v+1)`).
fn deinterleave(a: &Interleaved2D<i32>) -> (BandPlane, BandPlane, BandPlane, BandPlane) {
    let (w, h) = (a.width, a.height);
    let (llw, llh) = (w.div_ceil(2), h.div_ceil(2));
    let (hw, hh) = (w / 2, h / 2);
    let mut ll = vec![0i32; llw * llh];
    let mut hl = vec![0i32; hw * llh];
    let mut lh = vec![0i32; llw * hh];
    let mut hhb = vec![0i32; hw * hh];
    for v in 0..h {
        for u in 0..w {
            let s = a.data[v * w + u];
            match (u % 2, v % 2) {
                (0, 0) => ll[(v / 2) * llw + u / 2] = s,
                (1, 0) => hl[(v / 2) * hw + u / 2] = s,
                (0, 1) => lh[(v / 2) * llw + u / 2] = s,
                (1, 1) => hhb[(v / 2) * hw + u / 2] = s,
                _ => unreachable!(),
            }
        }
    }
    (
        BandPlane {
            width: llw,
            height: llh,
            data: ll,
        },
        BandPlane {
            width: hw,
            height: llh,
            data: hl,
        },
        BandPlane {
            width: llw,
            height: hh,
            data: lh,
        },
        BandPlane {
            width: hw,
            height: hh,
            data: hhb,
        },
    )
}

/// Run the `NL`-level §F.4 forward 5-3 cascade over one DC-shifted
/// component plane.
fn forward_cascade(samples: Vec<i32>, width: usize, height: usize, nl: u8) -> ComponentBands {
    let mut cur = BandPlane {
        width,
        height,
        data: samples,
    };
    let mut high = Vec::with_capacity(nl as usize);
    for _lev in 1..=nl {
        let a = sd_2d_5x3(cur.data, cur.width, cur.height, 0, 0)
            .expect("analysis dims match by construction");
        let (ll, hl, lh, hh) = deinterleave(&a);
        high.push([hl, lh, hh]);
        cur = ll;
    }
    ComponentBands { ll: cur, high }
}

/// §F.3.3 deinterleave for the real-valued 9-7 lattice, quantising each
/// sub-band sample to its Equation E-1 signed integer on the way out:
/// `qb = sign(y) · ⌊|y| / Δb⌋`, with the **uniform** step
/// `Δb = 2^(Rb − εb) = 2^(−fine_bits)` that the `εb = Rb + fine_bits`
/// exponent choice produces for every band (µb = 0).
fn deinterleave_quantise(
    a: &crate::dwt::Interleaved2D<f64>,
    scale: f64,
) -> (BandPlane, BandPlane, BandPlane, BandPlane) {
    let (w, h) = (a.width, a.height);
    let (llw, llh) = (w.div_ceil(2), h.div_ceil(2));
    let (hw, hh) = (w / 2, h / 2);
    let q = |y: f64| -> i32 {
        let m = (y.abs() * scale).floor() as i32;
        if y < 0.0 {
            -m
        } else {
            m
        }
    };
    let mut ll = vec![0i32; llw * llh];
    let mut hl = vec![0i32; hw * llh];
    let mut lh = vec![0i32; llw * hh];
    let mut hhb = vec![0i32; hw * hh];
    for v in 0..h {
        for u in 0..w {
            let s = q(a.data[v * w + u]);
            match (u % 2, v % 2) {
                (0, 0) => ll[(v / 2) * llw + u / 2] = s,
                (1, 0) => hl[(v / 2) * hw + u / 2] = s,
                (0, 1) => lh[(v / 2) * llw + u / 2] = s,
                (1, 1) => hhb[(v / 2) * hw + u / 2] = s,
                _ => unreachable!(),
            }
        }
    }
    (
        BandPlane {
            width: llw,
            height: llh,
            data: ll,
        },
        BandPlane {
            width: hw,
            height: llh,
            data: hl,
        },
        BandPlane {
            width: llw,
            height: hh,
            data: lh,
        },
        BandPlane {
            width: hw,
            height: hh,
            data: hhb,
        },
    )
}

/// Run the `NL`-level §F.4 forward **9-7** cascade over one DC-shifted
/// component plane, quantising every emitted sub-band per Annex E with
/// the uniform `Δb = 2^(−fine_bits)` step. The recursion continues on
/// the **unquantised** real LL (only the emitted bands quantise), so
/// deeper levels see full precision.
fn forward_cascade_9x7(
    samples: Vec<f64>,
    width: usize,
    height: usize,
    nl: u8,
    fine_bits: u8,
) -> ComponentBands {
    let scale = f64::from(1u32 << fine_bits);
    struct RealPlane {
        width: usize,
        height: usize,
        data: Vec<f64>,
    }
    let mut cur = RealPlane {
        width,
        height,
        data: samples,
    };
    let mut high = Vec::with_capacity(nl as usize);
    for _lev in 1..=nl {
        let a = crate::dwt::sd_2d_9x7(cur.data, cur.width, cur.height, 0, 0)
            .expect("analysis dims match by construction");
        let (_llq, hl, lh, hh) = deinterleave_quantise(&a, scale);
        // Real-valued LL for the next level (even/even lattice sites).
        let (llw, llh) = (a.width.div_ceil(2), a.height.div_ceil(2));
        let mut ll = vec![0f64; llw * llh];
        for v in 0..llh {
            for u in 0..llw {
                ll[v * llw + u] = a.data[(2 * v) * a.width + 2 * u];
            }
        }
        high.push([hl, lh, hh]);
        cur = RealPlane {
            width: llw,
            height: llh,
            data: ll,
        };
    }
    // Quantise the final LL.
    let q = |y: f64| -> i32 {
        let m = (y.abs() * scale).floor() as i32;
        if y < 0.0 {
            -m
        } else {
            m
        }
    };
    let ll = BandPlane {
        width: cur.width,
        height: cur.height,
        data: cur.data.iter().map(|&y| q(y)).collect(),
    };
    ComponentBands { ll, high }
}

/// One tier-1-encoded code-block ready for packet assembly.
struct EncodedBlock {
    /// §B.10.5 zero-bit-plane count `P = Mb − planes`.
    zero_bit_planes: u32,
    /// §B.10.6 coding passes (`3 · planes − 2`), zero for an all-zero
    /// (not included) block.
    coding_passes: u32,
    /// The single §C.3 codeword segment.
    bytes: Vec<u8>,
    /// Annex J.13.4 per-pass truncation rates `R^n` (byte length of a
    /// terminated segment covering passes `1..=n`), one entry per pass;
    /// empty when the caller did not request them (single layer).
    pass_rates: Vec<u32>,
    /// Annex J.13.4 per-pass distortions `D^n` (unweighted squared
    /// error under the midpoint-reconstruction model), one entry per
    /// pass; empty unless rate control requested them.
    pass_dist: Vec<f64>,
    /// `D^0` — distortion of skipping the block entirely (Σ m²).
    d0: f64,
    /// J.13.4.1 sub-band weight γ² (synthesis-waveform L2 norm squared,
    /// both axes), folded into the rate-distortion slopes.
    weight: f64,
    /// Global code-block ordinal — indexes the rate-control truncation
    /// vector.
    ordinal: usize,
    /// Re-encode context for exact §C.2.9 termination at a truncation
    /// point: `(orientation, width, height, coefficients, mb)`. Only
    /// kept when rate control is active.
    reencode: Option<(SubBandOrientation, usize, usize, Vec<Coefficient>, u32)>,
}

/// Linearised 5-3 synthesis of one interleaved level (T.800 §F.4.4
/// lifting steps with the rounding terms dropped), used only for the
/// Annex J.13.4.1 sub-band weight computation. `y[2n]` carries the
/// low-pass and `y[2n+1]` the high-pass coefficients (§F.3.1, `i0 = 0`);
/// boundary handling is the same §F.3.7 PSEO the real transform uses.
fn synth_5x3_linear(y: &[f64]) -> Vec<f64> {
    let n = y.len() as i32;
    let at = |v: &[f64], i: i32| v[crate::dwt::pseo(i, 0, n) as usize];
    let mut x = vec![0.0f64; y.len()];
    let mut i = 0i32;
    while i < n {
        x[i as usize] = at(y, i) - 0.25 * (at(y, i - 1) + at(y, i + 1));
        i += 2;
    }
    let mut i = 1i32;
    while i < n {
        let left = crate::dwt::pseo(i - 1, 0, n) as usize;
        let right = crate::dwt::pseo(i + 1, 0, n) as usize;
        x[i as usize] = at(y, i) + 0.5 * (x[left] + x[right]);
        i += 2;
    }
    x
}

/// 1-D synthesis-waveform L2 norm of a coefficient sitting `levels`
/// decompositions deep, entering the cascade on the high-pass branch
/// when `first_high` (T.800 Annex J.13.4.1: "computed from the L2 norm
/// of the relevant sub-band's wavelet synthesis waveform"). Runs an
/// impulse through this crate's own synthesis (`idwt_1d_9x7`, or the
/// linearised 5-3 above) level by level; deeper than 10 levels the
/// norm has converged geometrically, so the depth is clamped.
fn synth_gain_1d(levels: u8, first_high: bool, reversible: bool) -> f64 {
    let levels = levels.min(10);
    if levels == 0 {
        return 1.0;
    }
    let mut cur = vec![0.0f64; 16];
    cur[8] = 1.0;
    for lev in 0..levels {
        let n = cur.len();
        let mut y = vec![0.0f64; 2 * n];
        let odd = lev == 0 && first_high;
        for (i, &c) in cur.iter().enumerate() {
            y[2 * i + usize::from(odd)] = c;
        }
        if reversible {
            cur = synth_5x3_linear(&y);
        } else {
            let mut x = vec![0.0f64; 2 * n];
            crate::dwt::idwt_1d_9x7(&y, &mut x, 0, (2 * n) as i32)
                .expect("power-of-two impulse lattice");
            cur = x;
        }
    }
    cur.iter().map(|v| v * v).sum::<f64>().sqrt()
}

/// J.13.4.1 sub-band weight γ² for band (r, orientation) of an
/// `NL`-level decomposition: the separable product of the two 1-D
/// synthesis-waveform norms, squared.
fn band_synthesis_weight(reversible: bool, nl: u8, r: u8, orientation: SubBandOrientation) -> f64 {
    let levels = if r == 0 { nl } else { nl - r + 1 };
    let high_x = matches!(orientation, SubBandOrientation::HL | SubBandOrientation::HH);
    let high_y = matches!(orientation, SubBandOrientation::LH | SubBandOrientation::HH);
    let g = synth_gain_1d(levels, high_x, reversible) * synth_gain_1d(levels, high_y, reversible);
    g * g
}

/// Tier-1 encode one code-block's coefficients through the full §D.3
/// schedule into one codeword segment. `mb` is the Equation E-2 budget
/// for the block's sub-band. Returns `None` for an all-zero block (not
/// included in any packet).
///
/// What [`encode_code_block`] captures at each pass boundary, plus an
/// optional cap on the coded pass count (used by the PCRD re-encode to
/// terminate exactly at a truncation point).
#[derive(Debug, Clone, Copy, Default)]
struct PassCapture {
    /// Record the Annex J.13.4 truncation-point rates `R^n` — for each
    /// pass `n` (1-based) the byte length a §C.2.9-terminated segment
    /// covering passes `1..=n` would have, obtained by flushing a
    /// snapshot of the encoder state at that pass boundary. Multi-layer
    /// assembly cuts the final segment at these lengths, so a decoder
    /// that stops after an intermediate layer holds (almost exactly)
    /// the terminated prefix the snapshot would have produced.
    rates: bool,
    /// Record the Annex J.13.4 per-pass distortions `D^n`.
    dist: bool,
    /// Encode only the first `n` passes of the §D.3 schedule.
    max_passes: Option<u32>,
}

fn encode_code_block(
    orientation: SubBandOrientation,
    width: usize,
    height: usize,
    targets: &[Coefficient],
    mb: u32,
    capture: PassCapture,
) -> Result<Option<EncodedBlock>, Error> {
    let pass_rates = capture.rates;
    let pass_dist = capture.dist;
    let max_passes = capture.max_passes;
    let maxmag = targets.iter().map(|c| c.magnitude).max().unwrap_or(0);
    if maxmag == 0 {
        return Ok(None);
    }
    let planes = 32 - maxmag.leading_zeros();
    if planes > mb {
        // The εb / guard-bit budget cannot represent this coefficient —
        // with the §E.1.1 reversible exponents this cannot arise from
        // in-range input samples, so surface it as a defensive error
        // rather than emitting a stream the decoder would reject.
        return Err(Error::NotImplemented);
    }
    let top = planes - 1;
    let total_passes = 3 * planes - 2;
    let limit = max_passes.unwrap_or(total_passes).min(total_passes);
    if limit == 0 {
        return Ok(None);
    }
    let mut enc_block = CodeBlock::new(orientation, width, height);
    let mut encoder = MqEncoder::new();
    let mut ctx = reset_contexts();
    let mut rates: Vec<u32> = Vec::new();
    let mut dists: Vec<f64> = Vec::new();
    // Annex J.13.4 distortion under the §E.1.1.2 midpoint (r = 0.5)
    // reconstruction model: after `done` completed planes a
    // coefficient's magnitude is known down to plane `t = planes −
    // done`, so the decoder reconstructs `known + 2^(t−1)` (or exactly
    // `m` once t = 0, or 0 while still insignificant).
    let dist_of = |blk: &CodeBlock| -> f64 {
        let bits = blk.decoded_bits_raw();
        let mut d = 0.0;
        for (i, c) in targets.iter().enumerate() {
            let m = c.magnitude;
            if m == 0 {
                continue;
            }
            let done = bits[i].min(planes);
            let t = planes - done;
            let known = (m >> t) << t;
            let rec = if known == 0 {
                0.0
            } else if t == 0 {
                f64::from(m)
            } else {
                f64::from(known) + f64::from(1u32 << (t - 1))
            };
            let e = f64::from(m) - rec;
            d += e * e;
        }
        d
    };
    let mut coded = 0u32;
    // One §D.3 schedule step bookkeeping: snapshot the J.13.4 rate and
    // distortion at every pass boundary.
    macro_rules! after_pass {
        () => {{
            coded += 1;
            if pass_rates {
                rates.push(encoder.clone().flush().len() as u32);
            }
            if pass_dist {
                dists.push(dist_of(&enc_block));
            }
            coded == limit
        }};
    }
    'schedule: {
        enc_block.cleanup_encode(top, targets, &mut encoder, &mut ctx);
        if after_pass!() {
            break 'schedule;
        }
        for p in (0..top).rev() {
            enc_block.significance_propagation_encode(p, targets, &mut encoder, &mut ctx);
            if after_pass!() {
                break 'schedule;
            }
            enc_block.magnitude_refinement_encode(p, targets, &mut encoder, &mut ctx);
            if after_pass!() {
                break 'schedule;
            }
            enc_block.cleanup_encode(p, targets, &mut encoder, &mut ctx);
            if after_pass!() {
                break 'schedule;
            }
        }
    }
    debug_assert_eq!(coded, limit);
    let bytes = encoder.flush();
    if pass_rates {
        // The last snapshot and the final flush share the same encoder
        // state, so they agree; pin it exactly regardless.
        *rates.last_mut().expect("at least one pass") = bytes.len() as u32;
    }
    let d0 = if pass_dist {
        targets
            .iter()
            .map(|c| f64::from(c.magnitude) * f64::from(c.magnitude))
            .sum()
    } else {
        0.0
    };
    Ok(Some(EncodedBlock {
        zero_bit_planes: mb - planes,
        coding_passes: limit,
        bytes,
        pass_rates: rates,
        pass_dist: dists,
        d0,
        weight: 1.0,
        ordinal: 0,
        reencode: None,
    }))
}

/// Append a marker segment: marker code, 16-bit length (payload + 2),
/// payload.
fn push_segment(out: &mut Vec<u8>, marker: u16, payload: &[u8]) {
    out.extend_from_slice(&marker.to_be_bytes());
    out.extend_from_slice(&((payload.len() as u16 + 2).to_be_bytes()));
    out.extend_from_slice(payload);
}

/// Encode 8-bit unsigned component planes (all `width × height`, 1:1
/// sub-sampling) into a **lossless** Part-1 J2K codestream.
///
/// * `planes` — one row-major `width * height` sample plane per
///   component (1..=16384 components; every plane the same size).
/// * `nl` — decomposition levels `NL` (0..=32).
/// * `cb_exp` — `(xcb, ycb)` real code-block exponents (each `2..=10`,
///   sum ≤ 12 per Table A.18).
///
/// The output decodes bit-exactly back to `planes` through
/// [`crate::decode::decode_j2k`] (validated by the round-trip tests).
pub fn encode_j2k_lossless(
    planes: &[&[u8]],
    width: u32,
    height: u32,
    nl: u8,
    cb_exp: (u8, u8),
) -> Result<Vec<u8>, Error> {
    encode_j2k(
        planes,
        width,
        height,
        &EncodeParams {
            decomposition_levels: nl,
            code_block_exp: cb_exp,
            ..EncodeParams::default()
        },
    )
}

/// How the encoder transforms and quantises the sample planes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeKernel {
    /// Reversible 5-3, Table A.28 style 0 (no quantization) — lossless.
    Lossless5x3,
    /// Irreversible 9-7 with Annex E scalar-expounded quantisation
    /// (Table A.28 style 2) — lossy. `fine_bits` sets the uniform step
    /// `Δb = 2^(−fine_bits)` via `εb = Rb + fine_bits` (µb = 0): larger
    /// is finer / nearer-lossless (0..=8), `0` is a coarse `Δb = 1`.
    Lossy9x7 {
        /// Uniform quantisation-step fineness: `Δb = 2^(−fine_bits)`.
        fine_bits: u8,
    },
}

/// Structured encoder parameters — the T.800 §A.6.1 COD fields the
/// encoder honours, with spec-shaped defaults.
///
/// Build one with [`EncodeParams::default`] and override the fields of
/// interest, then call [`encode_j2k`]. The convenience wrappers
/// ([`encode_j2k_lossless`], [`encode_j2k_lossless_rct`],
/// [`encode_j2k_lossy`], [`encode_j2k_lossy_ict`]) construct one
/// internally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeParams {
    /// `NL` — wavelet decomposition levels (SPcod, Table A.15;
    /// `0..=32`). Default `3`.
    pub decomposition_levels: u8,
    /// `(xcb, ycb)` — real code-block exponents (Table A.18: each
    /// `2..=10`, sum ≤ 12). Default `(6, 6)` (64×64 code-blocks).
    pub code_block_exp: (u8, u8),
    /// Wavelet kernel + quantisation family (Table A.20 / Table A.28).
    /// Default [`EncodeKernel::Lossless5x3`].
    pub kernel: EncodeKernel,
    /// `SGcod` multiple-component-transformation flag (Table A.17):
    /// pairs the §G.2 RCT with the 5-3 kernel and the §G.3.1 ICT with
    /// the 9-7 kernel, across components 0–2 (requires exactly three
    /// planes). Default `false`.
    pub mct: bool,
    /// `SGcod` progression order (Table A.16) the tile's packets are
    /// emitted in — any of the five §B.12.1 orders. Default LRCP.
    pub progression: ProgressionOrder,
    /// User-defined precinct partition (T.800 Table A.21, `Scod` low
    /// bit): one byte per resolution level `r = 0..=NL` in order, low
    /// nibble `PPx`, high nibble `PPy` (the §B.6 partition at `r > 0`
    /// spans `2^(PPx−1)` sub-band samples, so `0` nibbles are only
    /// legal at `r = 0` per the Table A.21 note). **Empty** (the
    /// default) selects maximum precincts (`PPx = PPy = 15`,
    /// `Scod` bit clear).
    pub precincts: Vec<u8>,
    /// `SGcod` number of quality layers `L` (Table A.14, `1..=65535`).
    /// Each code-block's coding passes are distributed across the
    /// layers by coded depth (most-significant bit-planes first), and
    /// its single codeword segment is cut at the Annex J.13.4 per-pass
    /// truncation rates, so discarding trailing layers degrades the
    /// image gracefully (SNR scalability, J.13.2) while decoding every
    /// layer reproduces the single-layer result exactly. Default `1`.
    pub layers: u16,
    /// PCRD rate control (T.800 Annex J.13.3): a whole-codestream byte
    /// budget. When set, each code-block's embedded bit-stream is
    /// truncated at a coding-pass boundary chosen by the Lagrangian
    /// slope optimisation of Equation J-13 — the rates `R^n` are the
    /// Annex J.13.4 per-pass truncation lengths, the distortions `D^n`
    /// are midpoint-reconstruction squared errors weighted by the
    /// sub-band synthesis-waveform L2 norm (J.13.4.1), and the slope
    /// threshold λ is searched so the assembled stream is the largest
    /// one not exceeding the budget. Truncated code-blocks are
    /// re-encoded so the emitted codeword segment is exactly
    /// §C.2.9-terminated. A budget below the irreducible marker +
    /// empty-packet cost yields the smallest legal stream (best
    /// effort). Composes with `layers` (the layer split then divides
    /// the *retained* passes). Default `None` (no rate control).
    pub target_bytes: Option<usize>,
}

impl Default for EncodeParams {
    fn default() -> Self {
        EncodeParams {
            decomposition_levels: 3,
            code_block_exp: (6, 6),
            kernel: EncodeKernel::Lossless5x3,
            mct: false,
            progression: ProgressionOrder::Lrcp,
            precincts: Vec::new(),
            layers: 1,
            target_bytes: None,
        }
    }
}

/// Encode 8-bit unsigned component planes into a **lossy** Part-1 J2K
/// codestream using the irreversible 9-7 kernel (T.800 §F.4.8.2) and
/// Annex E scalar-expounded quantisation (Table A.28 style 2).
///
/// `fine_bits` (0..=8) sets the uniform quantisation step
/// `Δb = 2^(−fine_bits)` through the exponent choice
/// `εb = Rb + fine_bits` (µb = 0): `6` is near-lossless (decoded
/// samples within ±1 of the input), `0` is a coarse `Δb = 1`. The other
/// parameters match [`encode_j2k_lossless`].
pub fn encode_j2k_lossy(
    planes: &[&[u8]],
    width: u32,
    height: u32,
    nl: u8,
    cb_exp: (u8, u8),
    fine_bits: u8,
) -> Result<Vec<u8>, Error> {
    encode_j2k(
        planes,
        width,
        height,
        &EncodeParams {
            decomposition_levels: nl,
            code_block_exp: cb_exp,
            kernel: EncodeKernel::Lossy9x7 { fine_bits },
            ..EncodeParams::default()
        },
    )
}

/// Encode exactly three 8-bit RGB planes into a **lossy** Part-1 J2K
/// codestream with the §G.3.1 **irreversible component transform**
/// (ICT, `SGcod` MCT = 1 paired with the 9-7 kernel per Table A.17).
///
/// The DC level-shifted planes go through the Equation G-9/G-10/G-11
/// forward ICT before each component's 9-7 cascade; the decoder's
/// §G.3.2 inverse (Equations G-12 – G-14) restores RGB. Unlike the
/// §G.2 RCT, the ICT chrominance components stay within the luminance
/// dynamic range (the G-10/G-11 rows have unit ℓ1 gain on
/// full-range input), so all three components share the QCD exponents.
/// For correlated RGB the ICT concentrates signal energy into `Y0`
/// and the stream is smaller than three independently coded planes at
/// the same `fine_bits`. The other parameters match
/// [`encode_j2k_lossy`].
pub fn encode_j2k_lossy_ict(
    planes: &[&[u8]; 3],
    width: u32,
    height: u32,
    nl: u8,
    cb_exp: (u8, u8),
    fine_bits: u8,
) -> Result<Vec<u8>, Error> {
    encode_j2k(
        planes.as_slice(),
        width,
        height,
        &EncodeParams {
            decomposition_levels: nl,
            code_block_exp: cb_exp,
            kernel: EncodeKernel::Lossy9x7 { fine_bits },
            mct: true,
            ..EncodeParams::default()
        },
    )
}

/// Encode exactly three 8-bit RGB planes into a **lossless** Part-1 J2K
/// codestream with the §G.2 **reversible component transform** (RCT,
/// `SGcod` MCT = 1, Table A.17).
///
/// The DC-shifted planes go through the Equation G-3/G-4/G-5 forward
/// RCT before the 5-3 cascade; the chrominance components carry one
/// extra bit of dynamic range (§G.2), signalled through main-header
/// `QCC` markers whose exponents use `RI + 1` (picked up by the
/// decoder's §A.6.5 `Main QCC over Main QCD` precedence). For
/// correlated RGB input the RCT stream is smaller than three
/// independent planes; decoding is still bit-exact.
pub fn encode_j2k_lossless_rct(
    planes: &[&[u8]; 3],
    width: u32,
    height: u32,
    nl: u8,
    cb_exp: (u8, u8),
) -> Result<Vec<u8>, Error> {
    encode_j2k(
        planes.as_slice(),
        width,
        height,
        &EncodeParams {
            decomposition_levels: nl,
            code_block_exp: cb_exp,
            mct: true,
            ..EncodeParams::default()
        },
    )
}

/// Encode 8-bit unsigned component planes (all `width × height`, 1:1
/// sub-sampling) into a Part-1 J2K codestream per `params`.
///
/// * `planes` — one row-major `width * height` sample plane per
///   component (1..=16384 components; every plane the same size).
///
/// With the (default) reversible 5-3 kernel the output decodes back
/// **bit-exactly**; the 9-7 kernel quantises per Annex E. See
/// [`EncodeParams`] for the coding-style knobs.
pub fn encode_j2k(
    planes: &[&[u8]],
    width: u32,
    height: u32,
    params: &EncodeParams,
) -> Result<Vec<u8>, Error> {
    let nl = params.decomposition_levels;
    let (xcb, ycb) = params.code_block_exp;
    let kernel = params.kernel;
    let mct = params.mct;
    // Table A.17: MCT = 1 pairs the §G.2 RCT with the 5-3 kernel and
    // the §G.3.1 ICT with the 9-7 kernel, always across components 0–2.
    let use_rct = mct && matches!(kernel, EncodeKernel::Lossless5x3);
    let use_ict = mct && matches!(kernel, EncodeKernel::Lossy9x7 { .. });
    if mct && planes.len() != 3 {
        return Err(Error::NotImplemented);
    }
    if let EncodeKernel::Lossy9x7 { fine_bits } = kernel {
        if fine_bits > 8 {
            return Err(Error::NotImplemented);
        }
    }
    // Table A.16: only the five defined §B.12.1 orders are encodable.
    let progression_byte: u8 = match params.progression {
        ProgressionOrder::Lrcp => 0x00,
        ProgressionOrder::Rlcp => 0x01,
        ProgressionOrder::Rpcl => 0x02,
        ProgressionOrder::Pcrl => 0x03,
        ProgressionOrder::Cprl => 0x04,
        ProgressionOrder::Reserved(_) => return Err(Error::NotImplemented),
    };
    if planes.is_empty()
        || width == 0
        || height == 0
        || nl > 32
        || !(2..=10).contains(&xcb)
        || !(2..=10).contains(&ycb)
        || xcb + ycb > 12
    {
        return Err(Error::NotImplemented);
    }
    // Table A.14: at least one quality layer.
    if params.layers == 0 {
        return Err(Error::NotImplemented);
    }
    // Table A.21: when user-defined precincts are signalled the COD
    // carries exactly NL + 1 bytes, and a zero PPx / PPy nibble is only
    // permitted at the lowest resolution level (r = 0).
    if !params.precincts.is_empty() {
        if params.precincts.len() != nl as usize + 1 {
            return Err(Error::NotImplemented);
        }
        for &b in &params.precincts[1..] {
            if b & 0x0F == 0 || (b >> 4) & 0x0F == 0 {
                return Err(Error::NotImplemented);
            }
        }
    }
    let n = (width as usize)
        .checked_mul(height as usize)
        .ok_or(Error::InvalidMarkerLength)?;
    for p in planes {
        if p.len() != n {
            return Err(Error::InvalidMarkerLength);
        }
    }
    const PRECISION: u8 = 8;

    // -- SIZ model (drives both the marker bytes and the geometry) ----
    let siz = Siz {
        rsiz: 0,
        x_size: width,
        y_size: height,
        x_offset: 0,
        y_offset: 0,
        tile_width: width,
        tile_height: height,
        tile_x_offset: 0,
        tile_y_offset: 0,
        components: planes
            .iter()
            .map(|_| SizComponent {
                precision_bits: PRECISION,
                is_signed: false,
                h_separation: 1,
                v_separation: 1,
            })
            .collect(),
    };

    // -- Forward transform per component ------------------------------
    // §G.1.2 DC level shift, then (optionally) the Table A.17 forward
    // MCT across components 0–2 (§G.2 RCT with the 5-3 kernel, §G.3.1
    // ICT with the 9-7 kernel), then the per-component §F.4 cascade —
    // integer 5-3 for the lossless kernel, real-valued 9-7 with Annex E
    // quantisation for the lossy kernel.
    let dc = 1i32 << (PRECISION - 1);
    let bands: Vec<ComponentBands> = match kernel {
        EncodeKernel::Lossless5x3 => {
            let mut shifted: Vec<Vec<i32>> = planes
                .iter()
                .map(|p| p.iter().map(|&s| s as i32 - dc).collect())
                .collect();
            if use_rct {
                // Split into three disjoint &mut [i32] (MSRV-friendly).
                let (head, tail) = shifted.split_at_mut(1);
                let (mid, tail2) = tail.split_at_mut(1);
                crate::mct::forward_rct(&mut head[0], &mut mid[0], &mut tail2[0])?;
            }
            shifted
                .into_iter()
                .map(|p| forward_cascade(p, width as usize, height as usize, nl))
                .collect()
        }
        EncodeKernel::Lossy9x7 { fine_bits } => {
            let mut shifted: Vec<Vec<f64>> = planes
                .iter()
                .map(|p| p.iter().map(|&s| f64::from(s) - f64::from(dc)).collect())
                .collect();
            if use_ict {
                let (head, tail) = shifted.split_at_mut(1);
                let (mid, tail2) = tail.split_at_mut(1);
                crate::mct::forward_ict_f64(&mut head[0], &mut mid[0], &mut tail2[0])?;
            }
            shifted
                .into_iter()
                .map(|p| forward_cascade_9x7(p, width as usize, height as usize, nl, fine_bits))
                .collect()
        }
    };

    // -- Geometry (shared with the decoder) ---------------------------
    let tile = derive_tile_geometry(&siz, 0)?;
    let levels_per_comp: Vec<_> = tile
        .components
        .iter()
        .map(|tc| derive_resolution_levels(*tc, nl))
        .collect();

    // -- Tier-1: encode every code-block, keyed (comp, r, precinct) ---
    // Per precinct (comp, r, k): the per-sub-band code-block grids +
    // zero-bit-plane leaves, and the encoded blocks in §B.10.8 order
    // (`None` = empty grid cell or all-zero block, never included).
    struct PrecinctRaw {
        sub_bands: Vec<(SubBandGeometry, Vec<u32>)>,
        blocks: Vec<Option<EncodedBlock>>,
    }
    use std::collections::BTreeMap;
    let layer_count = params.layers;
    let rate_control = params.target_bytes.is_some();
    let want_rates = layer_count > 1 || rate_control;
    let mut packets: BTreeMap<(u16, u8, u32), PrecinctRaw> = BTreeMap::new();
    let mut num_blocks = 0usize;
    // Deepest coded depth across all code-blocks (zero-bit-planes plus
    // coded plane ordinal) — the J.13.2-style layer split aligns layer
    // boundaries on this global bit-plane depth scale.
    let mut max_depth = 0u32;
    let mut precincts_per_comp_res: Vec<Vec<u32>> = Vec::with_capacity(planes.len());
    // Parallel per-component input for the position-keyed (RPCL / PCRL /
    // CPRL) §B.12.1.3–5 orders: the precinct grids plus the §B.6
    // reference-grid corner mapping those orders sort visits by.
    let mut position_infos: Vec<ComponentPositionInfo> = Vec::with_capacity(planes.len());

    for (ci, levels) in levels_per_comp.iter().enumerate() {
        let mut precincts_at_r = Vec::with_capacity(levels.len());
        let mut res_layouts = Vec::with_capacity(levels.len());
        for level in levels {
            let pp = precinct_exponents_at(&params.precincts, level.r);
            let partition = derive_precinct_partition(level, pp);
            let num_precincts = partition.num_precincts() as u32;
            precincts_at_r.push(num_precincts);
            // §B.6: the precinct partition anchors at (0, 0) on the
            // reduced-resolution domain with step 2^PP; the level's
            // left/top edge falls in anchor cell floor(trx0 / 2^PPx).
            res_layouts.push(ResolutionPrecinctLayout {
                num_wide: partition.num_wide,
                num_high: partition.num_high,
                anchor_x: level.trx0 >> pp.ppx,
                anchor_y: level.try0 >> pp.ppy,
                trx0: level.trx0,
                try0: level.try0,
                ppx: pp.ppx,
                ppy: pp.ppy,
            });
            for k in 0..num_precincts {
                let pcb = derive_precinct_code_blocks(level, pp, xcb, ycb, k)?;
                let mut sub_bands: Vec<(SubBandGeometry, Vec<u32>)> = Vec::new();
                let mut blocks: Vec<Option<EncodedBlock>> = Vec::new();
                for psb in &pcb.sub_bands {
                    let geom = SubBandGeometry {
                        width: psb.grid_wide,
                        height: psb.grid_high,
                    };
                    let nblocks = (psb.grid_wide as usize) * (psb.grid_high as usize);
                    let mut zbp = vec![0u32; nblocks];
                    // Sub-band plane for (ci, r, orientation).
                    let plane: &BandPlane = if level.r == 0 {
                        &bands[ci].ll
                    } else {
                        let lev = (nl - level.r + 1) as usize; // nb = NL − r + 1
                        let oi = match psb.orientation {
                            SubBandOrientation::HL => 0,
                            SubBandOrientation::LH => 1,
                            SubBandOrientation::HH => 2,
                            SubBandOrientation::LL => return Err(Error::InvalidPacketHeader),
                        };
                        &bands[ci].high[lev - 1][oi]
                    };
                    // §G.2: RCT chrominance carries one extra bit of
                    // dynamic range, signalled via this component's QCC.
                    let ri = if use_rct && ci > 0 {
                        u32::from(PRECISION) + 1
                    } else {
                        u32::from(PRECISION)
                    };
                    // εb = Rb (+ fine_bits on the lossy path, where the
                    // exponent excess sets the Equation E-3 step).
                    let fine = match kernel {
                        EncodeKernel::Lossless5x3 => 0,
                        EncodeKernel::Lossy9x7 { fine_bits } => u32::from(fine_bits),
                    };
                    let eps = ri + u32::from(band_gain(psb.orientation)) + fine;
                    let mb = u32::from(GUARD_BITS) + eps - 1;
                    // J.13.4.1 rate-distortion weight of this band.
                    let weight = if rate_control {
                        band_synthesis_weight(
                            matches!(kernel, EncodeKernel::Lossless5x3),
                            nl,
                            level.r,
                            psb.orientation,
                        )
                    } else {
                        1.0
                    };
                    for (bi, cb) in psb.code_blocks.iter().enumerate() {
                        let (bw, bh) = (cb.width() as usize, cb.height() as usize);
                        if bw == 0 || bh == 0 {
                            blocks.push(None);
                            continue;
                        }
                        // Extract the block's coefficients from the band
                        // plane (absolute band coords; origin-0 image).
                        let mut targets = Vec::with_capacity(bw * bh);
                        for v in cb.y0..cb.y1 {
                            for u in cb.x0..cb.x1 {
                                let s = plane.data[(v as usize) * plane.width + u as usize];
                                targets.push(Coefficient {
                                    magnitude: s.unsigned_abs(),
                                    sigma: false,
                                    sign: s < 0,
                                    already_refined: false,
                                });
                            }
                        }
                        let mut enc = encode_code_block(
                            psb.orientation,
                            bw,
                            bh,
                            &targets,
                            mb,
                            PassCapture {
                                rates: want_rates,
                                dist: rate_control,
                                max_passes: None,
                            },
                        )?;
                        if let Some(enc) = &mut enc {
                            zbp[bi] = enc.zero_bit_planes;
                            // Depth of the block's final pass on the
                            // global bit-plane scale.
                            max_depth =
                                max_depth.max(enc.zero_bit_planes + (enc.coding_passes + 1) / 3);
                            enc.weight = weight;
                            enc.ordinal = num_blocks;
                            num_blocks += 1;
                            if rate_control {
                                enc.reencode = Some((psb.orientation, bw, bh, targets, mb));
                            }
                        }
                        blocks.push(enc);
                    }
                    sub_bands.push((geom, zbp));
                }
                packets.insert((ci as u16, level.r, k), PrecinctRaw { sub_bands, blocks });
            }
        }
        precincts_per_comp_res.push(precincts_at_r);
        position_infos.push(ComponentPositionInfo {
            num_decomposition_levels: nl,
            xrsiz: 1,
            yrsiz: 1,
            resolutions: res_layouts,
        });
    }

    // -- Tier-2 packet order (computed once, reused per assembly) ------
    let prog_info: Vec<ComponentProgressionInfo> = precincts_per_comp_res
        .iter()
        .map(|pr| ComponentProgressionInfo {
            num_decomposition_levels: nl,
            precincts_per_resolution: pr.clone(),
        })
        .collect();
    let order = match params.progression {
        ProgressionOrder::Lrcp => lrcp_packet_order(layer_count, &prog_info)?,
        ProgressionOrder::Rlcp => rlcp_packet_order(layer_count, &prog_info)?,
        ProgressionOrder::Rpcl => rpcl_packet_order(layer_count, &position_infos)?,
        ProgressionOrder::Pcrl => pcrl_packet_order(layer_count, &position_infos)?,
        ProgressionOrder::Cprl => cprl_packet_order(layer_count, &position_infos)?,
        ProgressionOrder::Reserved(_) => return Err(Error::NotImplemented),
    };

    // -- Assembly: layer split + tier-2 emission + markers -------------
    //
    // `trunc` optionally caps each block's included passes (indexed by
    // block ordinal — the PCRD rate-control choice); `exact` re-encodes
    // truncated blocks so their emitted codeword segment is exactly
    // §C.2.9-terminated (the λ search skips that and cuts at R^n, which
    // has the identical length).
    //
    // Layer split (T.800 §B.10.7.1 + Annex J.13.2 guidance): each
    // code-block's consecutive coding passes are distributed over the L
    // layers by their coded depth `P + ⌈i / 3⌉` (pass 0 is the first
    // cleanup, then SP / MR / cleanup triples per plane, §D.3); depth d
    // lands in layer ⌊d · L / (D + 1)⌋ where D is the deepest coded
    // depth in the tile. Most-significant planes therefore populate the
    // early layers across every code-block (the J.13.2 SNR-scalable
    // shape), and the block's single codeword segment is cut at the
    // Annex J.13.4 truncation rates R^n captured during tier-1.
    let layer_of_depth = |depth: u32| -> u16 {
        let l = u64::from(depth) * u64::from(layer_count) / (u64::from(max_depth) + 1);
        (l as u16).min(layer_count - 1)
    };
    struct LayeredBlock {
        zero_bit_planes: u32,
        /// Per layer: passes contributed + byte range of the segment.
        per_layer: Vec<(u32, std::ops::Range<usize>)>,
        bytes: Vec<u8>,
    }
    struct PrecinctLayered {
        state: PrecinctEncoderState,
        blocks: Vec<Option<LayeredBlock>>,
    }
    let assemble = |trunc: Option<&[u32]>, exact: bool| -> Result<Vec<u8>, Error> {
        let mut assembled: BTreeMap<(u16, u8, u32), PrecinctLayered> = BTreeMap::new();
        for (key, raw) in &packets {
            let mut blocks: Vec<Option<LayeredBlock>> = Vec::with_capacity(raw.blocks.len());
            // Per-sub-band first-inclusion layers (§B.10.4 tag trees).
            let mut first_layers: Vec<Vec<u32>> = raw
                .sub_bands
                .iter()
                .map(|(geom, _)| {
                    vec![u32::from(layer_count); (geom.width as usize) * (geom.height as usize)]
                })
                .collect();
            let mut sb_idx = 0usize;
            let mut in_band = 0usize;
            for enc in &raw.blocks {
                while in_band >= first_layers[sb_idx].len() {
                    sb_idx += 1;
                    in_band = 0;
                }
                let bi = in_band;
                in_band += 1;
                let Some(enc) = enc else {
                    blocks.push(None);
                    continue;
                };
                let n_eff =
                    trunc.map_or(enc.coding_passes, |t| t[enc.ordinal].min(enc.coding_passes));
                if n_eff == 0 {
                    // Rate control dropped the block entirely.
                    blocks.push(None);
                    continue;
                }
                // The block's (possibly truncated) codeword segment.
                let seg: Vec<u8> = if n_eff == enc.coding_passes {
                    enc.bytes.clone()
                } else if exact {
                    // Re-encode with §C.2.9 termination at the chosen
                    // truncation pass — same emitted bytes, exact tail.
                    let (o, bw, bh, tg, mb) =
                        enc.reencode.as_ref().expect("rate control keeps context");
                    let re = encode_code_block(
                        *o,
                        *bw,
                        *bh,
                        tg,
                        *mb,
                        PassCapture {
                            max_passes: Some(n_eff),
                            ..PassCapture::default()
                        },
                    )?
                    .expect("non-empty block re-encodes");
                    debug_assert_eq!(re.bytes.len() as u32, enc.pass_rates[n_eff as usize - 1]);
                    re.bytes
                } else {
                    let cut = (enc.pass_rates[n_eff as usize - 1] as usize).min(enc.bytes.len());
                    enc.bytes[..cut].to_vec()
                };
                let total = seg.len();
                // Count this block's passes per layer.
                let mut counts = vec![0u32; layer_count as usize];
                for i in 0..n_eff {
                    let depth = enc.zero_bit_planes + i.div_ceil(3);
                    counts[layer_of_depth(depth) as usize] += 1;
                }
                // Cut the segment at the per-pass truncation rates.
                let mut per_layer = Vec::with_capacity(layer_count as usize);
                let mut cum = 0u32;
                let mut prev = 0usize;
                let mut first = None;
                for (l, &p) in counts.iter().enumerate() {
                    if p > 0 && first.is_none() {
                        first = Some(l);
                    }
                    cum += p;
                    let end = if p == 0 {
                        prev
                    } else if cum == n_eff {
                        total
                    } else {
                        (enc.pass_rates[cum as usize - 1] as usize).clamp(prev, total)
                    };
                    per_layer.push((p, prev..end));
                    prev = end;
                }
                first_layers[sb_idx][bi] = first.expect("n_eff > 0 has a first layer") as u32;
                blocks.push(Some(LayeredBlock {
                    zero_bit_planes: enc.zero_bit_planes,
                    per_layer,
                    bytes: seg,
                }));
            }
            let sub_band_plans: Vec<SubBandEncoderPlan> = raw
                .sub_bands
                .iter()
                .cloned()
                .zip(first_layers)
                .map(|((geom, zbp), fl)| (geom, fl, zbp))
                .collect();
            assembled.insert(
                *key,
                PrecinctLayered {
                    state: PrecinctEncoderState::new(&sub_band_plans),
                    blocks,
                },
            );
        }

        // Tier-2: emit packets in the §B.12.1 order the COD signals.
        let mut tile_body: Vec<u8> = Vec::new();
        for desc in &order {
            let pa = assembled
                .get_mut(&(desc.component, desc.resolution, desc.precinct))
                .ok_or(Error::InvalidPacketHeader)?;
            let mut plans: Vec<CodeBlockPlan> = Vec::with_capacity(pa.blocks.len());
            let mut body: Vec<u8> = Vec::new();
            for b in &pa.blocks {
                match b {
                    None => plans.push(CodeBlockPlan {
                        included: false,
                        zero_bit_planes: 0,
                        coding_passes: 0,
                        segment_length: 0,
                    }),
                    Some(lb) => {
                        let (p, range) = &lb.per_layer[desc.layer as usize];
                        if *p == 0 {
                            plans.push(CodeBlockPlan {
                                included: false,
                                zero_bit_planes: lb.zero_bit_planes,
                                coding_passes: 0,
                                segment_length: 0,
                            });
                        } else {
                            plans.push(CodeBlockPlan {
                                included: true,
                                zero_bit_planes: lb.zero_bit_planes,
                                coding_passes: *p,
                                segment_length: range.len() as u32,
                            });
                            body.extend_from_slice(&lb.bytes[range.clone()]);
                        }
                    }
                }
            }
            let header = encode_packet_header(&mut pa.state, desc.layer, &plans);
            tile_body.extend_from_slice(&header);
            tile_body.extend_from_slice(&body);
        }

        // Markers.
        let mut out = Vec::new();
        out.extend_from_slice(&MARKER_SOC.to_be_bytes());

        // SIZ (Table A.9).
        let mut siz_payload = Vec::with_capacity(36 + 3 * planes.len());
        siz_payload.extend_from_slice(&siz.rsiz.to_be_bytes());
        for v in [
            siz.x_size,
            siz.y_size,
            siz.x_offset,
            siz.y_offset,
            siz.tile_width,
            siz.tile_height,
            siz.tile_x_offset,
            siz.tile_y_offset,
        ] {
            siz_payload.extend_from_slice(&v.to_be_bytes());
        }
        siz_payload.extend_from_slice(&(planes.len() as u16).to_be_bytes());
        for c in &siz.components {
            siz_payload.push(c.precision_bits - 1); // Ssiz: unsigned, depth − 1
            siz_payload.push(c.h_separation);
            siz_payload.push(c.v_separation);
        }
        push_segment(&mut out, MARKER_SIZ, &siz_payload);

        // COD (Tables A.13 – A.21): Scod bit 0 flags user-defined
        // precincts (whose Table A.21 bytes then trail SPcod), no
        // SOP / EPH, the signalled progression, the layer count, MCT
        // per Table A.17, NL levels, code-block exponents − 2, style 0,
        // and the Table A.20 kernel byte (1 = 5-3 reversible, 0 = 9-7
        // irreversible).
        let transform_byte = match kernel {
            EncodeKernel::Lossless5x3 => 1u8,
            EncodeKernel::Lossy9x7 { .. } => 0u8,
        };
        let scod = if params.precincts.is_empty() {
            0u8
        } else {
            0x01
        };
        let mut cod_payload = vec![
            scod,                       // Scod
            progression_byte,           // SGcod: progression (Table A.16)
            (layer_count >> 8) as u8,   // SGcod: layers (16-bit BE)
            (layer_count & 0xFF) as u8, // SGcod: layers, low byte
            mct as u8,                  // SGcod: MCT (Table A.17)
            nl,                         // SPcod: NL
            xcb - 2,                    // SPcod: xcb − 2
            ycb - 2,                    // SPcod: ycb − 2
            0,                          // SPcod: code-block style
            transform_byte,             // SPcod: transform (Table A.20)
        ];
        cod_payload.extend_from_slice(&params.precincts); // Table A.21
        push_segment(&mut out, MARKER_COD, &cod_payload);

        // QCD (Tables A.27 – A.28), one entry per sub-band in the
        // §F.3.1 order (NLLL then per-level HL, LH, HH from the deepest
        // level outward). `ri` is the component bit depth the exponents
        // build on.
        //
        // * Lossless: style 0 (no quantization) — one byte per band,
        //   `εb = RI + gain` in the top 5 bits.
        // * Lossy: style 2 (scalar expounded) — two bytes per band,
        //   `εb = Rb + fine_bits` in the top 5 bits, µb = 0
        //   (Table A.30), giving the uniform Equation E-3 step
        //   `Δb = 2^(−fine_bits)`.
        let quant_payload = |ri: u8| -> Vec<u8> {
            let mut p = Vec::new();
            match kernel {
                EncodeKernel::Lossless5x3 => {
                    p.push(GUARD_BITS << 5); // style 0 | guard bits
                    p.push(ri << 3); // εb(LL) = RI + 0
                    for _r in 1..=nl {
                        p.push((ri + 1) << 3); // HL: RI + 1
                        p.push((ri + 1) << 3); // LH: RI + 1
                        p.push((ri + 2) << 3); // HH: RI + 2
                    }
                }
                EncodeKernel::Lossy9x7 { fine_bits } => {
                    p.push((GUARD_BITS << 5) | 2); // style 2 | guard bits
                    let word = |gain: u8| -> [u8; 2] {
                        let eps = u16::from(ri + gain + fine_bits);
                        (eps << 11).to_be_bytes()
                    };
                    p.extend_from_slice(&word(0)); // LL
                    for _r in 1..=nl {
                        p.extend_from_slice(&word(1)); // HL
                        p.extend_from_slice(&word(1)); // LH
                        p.extend_from_slice(&word(2)); // HH
                    }
                }
            }
            p
        };
        push_segment(&mut out, MARKER_QCD, &quant_payload(PRECISION));
        if use_rct {
            // §G.2 / §A.6.5: the RCT chrominance components (1, 2)
            // carry one extra bit of dynamic range — override their
            // exponents with a main-header QCC each (`Main QCC > Main
            // QCD`). Cqcc is one byte (Csiz = 3 < 257).
            for c in 1u8..=2 {
                let mut qcc_payload = vec![c];
                qcc_payload.extend_from_slice(&quant_payload(PRECISION + 1));
                push_segment(&mut out, crate::MARKER_QCC, &qcc_payload);
            }
        }

        // SOT + SOD + tile body (§A.4.2): Psot spans SOT → end of body.
        let psot = 12u32 + 2 + tile_body.len() as u32;
        let mut sot_payload = Vec::with_capacity(8);
        sot_payload.extend_from_slice(&0u16.to_be_bytes()); // Isot
        sot_payload.extend_from_slice(&psot.to_be_bytes());
        sot_payload.push(0); // TPsot
        sot_payload.push(1); // TNsot
        push_segment(&mut out, MARKER_SOT, &sot_payload);
        out.extend_from_slice(&MARKER_SOD.to_be_bytes());
        out.extend_from_slice(&tile_body);

        out.extend_from_slice(&MARKER_EOC.to_be_bytes());
        Ok(out)
    };

    // -- PCRD rate control (T.800 Annex J.13.3) -------------------------
    let Some(target) = params.target_bytes else {
        return assemble(None, true);
    };
    let full = assemble(None, true)?;
    if full.len() <= target {
        return Ok(full);
    }
    // Per-block monotone-slope truncation sets N_i (J.13.3): the subset
    // of pass boundaries whose rate-distortion slopes S = ΔD / ΔR are
    // strictly decreasing, built from the anchor (R = 0, D = D^0).
    // Each entry is (passes, rate, weighted slope).
    let mut hulls: Vec<Vec<(u32, u32, f64)>> = vec![Vec::new(); num_blocks];
    let mut max_slope = 0.0f64;
    let mut min_slope = f64::INFINITY;
    for raw in packets.values() {
        for enc in raw.blocks.iter().flatten() {
            // Monotone hull over the (R^n, D^n) points.
            let mut pts: Vec<(u32, u32, f64)> = vec![(0, 0, enc.d0)];
            for n in 1..=enc.coding_passes as usize {
                let r = enc.pass_rates[n - 1];
                let d = enc.pass_dist[n - 1];
                if d + 1e-12 >= pts.last().expect("anchor").2 {
                    continue; // no distortion gain — never a truncation
                }
                // Same rate, lower distortion → the newer point wins.
                while pts.len() > 1 && r <= pts.last().expect("len>1").1 {
                    pts.pop();
                }
                // Keep slopes strictly decreasing along the hull.
                while pts.len() > 1 {
                    let last = *pts.last().expect("len>1");
                    let prev = pts[pts.len() - 2];
                    let s_last = (prev.2 - last.2) / f64::from(last.1 - prev.1).max(0.5);
                    let s_new = (last.2 - d) / f64::from(r.saturating_sub(last.1)).max(0.5);
                    if s_last <= s_new {
                        pts.pop();
                    } else {
                        break;
                    }
                }
                pts.push((n as u32, r, d));
            }
            let mut hull = Vec::with_capacity(pts.len().saturating_sub(1));
            for w in pts.windows(2) {
                let dr = f64::from(w[1].1.saturating_sub(w[0].1)).max(0.5);
                let slope = enc.weight * (w[0].2 - w[1].2) / dr;
                if slope > 0.0 {
                    max_slope = max_slope.max(slope);
                    min_slope = min_slope.min(slope);
                    hull.push((w[1].0, w[1].1, slope));
                }
            }
            hulls[enc.ordinal] = hull;
        }
    }
    // For a slope threshold λ, each block keeps the deepest truncation
    // point whose slope still exceeds λ (Equation J-13 minimiser).
    let pick = |lambda: f64| -> Vec<u32> {
        hulls
            .iter()
            .map(|h| {
                let mut n = 0u32;
                for &(p, _r, s) in h {
                    if s > lambda {
                        n = p;
                    } else {
                        break;
                    }
                }
                n
            })
            .collect()
    };
    if max_slope <= 0.0 || !min_slope.is_finite() {
        // Nothing coded anywhere — the minimal stream is the answer.
        return assemble(Some(&vec![0; num_blocks]), true);
    }
    // Bisect λ in log space: len(λ) is non-increasing in λ, so keep the
    // largest stream not exceeding the budget. J.13.3 notes the
    // residual gap is small (typically well under 100 bytes).
    let mut lo = min_slope.ln() - 1.0; // ≈ include everything
    let mut hi = max_slope.ln() + 1.0; // include nothing
    let mut best: Option<Vec<u32>> = None;
    for _ in 0..48 {
        let mid = 0.5 * (lo + hi);
        let trunc = pick(mid.exp());
        let s = assemble(Some(&trunc), false)?;
        if s.len() <= target {
            best = Some(trunc);
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let trunc = best.unwrap_or_else(|| vec![0; num_blocks]);
    assemble(Some(&trunc), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::decode_j2k;

    fn lcg(state: &mut u32) -> u32 {
        *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *state
    }

    /// Encode `planes`, decode with this crate's decoder, and assert the
    /// round-trip is bit-exact per component.
    fn roundtrip(planes: &[&[u8]], w: u32, h: u32, nl: u8, cb: (u8, u8)) {
        let stream = encode_j2k_lossless(planes, w, h, nl, cb).expect("encode");
        let img = decode_j2k(&stream).expect("decode own stream");
        assert_eq!(img.components.len(), planes.len());
        for (ci, (comp, plane)) in img.components.iter().zip(planes).enumerate() {
            assert_eq!(comp.width, w, "comp {ci} width");
            assert_eq!(comp.height, h, "comp {ci} height");
            assert_eq!(comp.precision_bits, 8);
            let got: Vec<u8> = comp.samples.iter().map(|&s| s as u8).collect();
            assert_eq!(&got[..], &plane[..], "comp {ci} samples");
        }
    }

    fn gradient(w: u32, h: u32) -> Vec<u8> {
        (0..w * h)
            .map(|i| (((i % w) * 5 + (i / w) * 3) % 256) as u8)
            .collect()
    }

    fn noise(w: u32, h: u32, seed: u32) -> Vec<u8> {
        let mut s = seed;
        (0..w * h).map(|_| (lcg(&mut s) >> 13) as u8).collect()
    }

    #[test]
    fn lossless_gray_nl0_single_block() {
        // NL = 0: the LL band is the raw plane; one code-block.
        let p = gradient(8, 8);
        roundtrip(&[&p], 8, 8, 0, (4, 4));
    }

    #[test]
    fn lossless_gray_nl1() {
        let p = gradient(16, 16);
        roundtrip(&[&p], 16, 16, 1, (4, 4));
    }

    #[test]
    fn lossless_gray_nl3_odd_dims() {
        // Odd dimensions exercise the ceil/floor band splits and the
        // PSEO parity at every cascade level.
        let p = gradient(37, 23);
        roundtrip(&[&p], 37, 23, 3, (4, 4));
    }

    #[test]
    fn lossless_gray_noise_multi_codeblock() {
        // 64×48 with 16×16 code-blocks → multi-block grids in every
        // band; noise stresses every coding-pass branch.
        let p = noise(64, 48, 0xA5A5_5A5A);
        roundtrip(&[&p], 64, 48, 2, (4, 4));
    }

    #[test]
    fn lossless_gray_extreme_values() {
        // All-0 / all-255 regions produce large DWT coefficients at the
        // boundary — exercises the deepest bit-planes and the Mb budget.
        let w = 32u32;
        let h = 32u32;
        let p: Vec<u8> = (0..w * h)
            .map(|i| if (i % w) < w / 2 { 0 } else { 255 })
            .collect();
        roundtrip(&[&p], w, h, 3, (4, 4));
    }

    #[test]
    fn lossless_gray_flat_image_empty_packets() {
        // A flat mid-grey plane: after the DC shift everything is zero,
        // every high band is all-zero (not-included code-blocks / empty
        // packets); only the LL DC survives... at value 0, so even the
        // LL block is excluded and every packet is empty.
        let p = vec![128u8; 24 * 24];
        roundtrip(&[&p], 24, 24, 2, (4, 4));
    }

    #[test]
    fn lossless_rgb_three_components() {
        // Three planes, no MCT: each component encodes independently.
        let r = gradient(21, 17);
        let g = noise(21, 17, 0x1357_2468);
        let b = vec![200u8; 21 * 17];
        roundtrip(&[&r, &g, &b], 21, 17, 2, (4, 4));
    }

    #[test]
    fn lossless_tiny_image() {
        // 1×1 and 2×3 degenerate geometries (empty high bands at some
        // levels, single-coefficient code-blocks).
        roundtrip(&[&[77u8]], 1, 1, 1, (4, 4));
        let p = vec![3u8, 250, 12, 99, 180, 42];
        roundtrip(&[&p], 2, 3, 2, (4, 4));
    }

    #[test]
    fn lossless_small_codeblocks() {
        // 4×4 code-blocks (the Table A.18 minimum) over a 20×20 noise
        // image: many small blocks per band.
        let p = noise(20, 20, 0xBEEF_CAFE);
        roundtrip(&[&p], 20, 20, 1, (2, 2));
    }

    #[test]
    fn encode_rejects_bad_params() {
        let p = vec![0u8; 16];
        // Empty planes.
        assert!(encode_j2k_lossless(&[], 4, 4, 1, (4, 4)).is_err());
        // Wrong plane size.
        assert!(encode_j2k_lossless(&[&p], 5, 4, 1, (4, 4)).is_err());
        // Code-block exponents out of Table A.18 range.
        assert!(encode_j2k_lossless(&[&p], 4, 4, 1, (1, 4)).is_err());
        assert!(encode_j2k_lossless(&[&p], 4, 4, 1, (10, 10)).is_err());
    }

    // -- §G.2 reversible component transform (MCT = 1) ----------------

    /// Encode three planes with the RCT, decode with this crate's
    /// decoder, and assert bit-exact recovery.
    fn roundtrip_rct(planes: &[&[u8]; 3], w: u32, h: u32, nl: u8, cb: (u8, u8)) -> usize {
        let stream = encode_j2k_lossless_rct(planes, w, h, nl, cb).expect("encode rct");
        let img = decode_j2k(&stream).expect("decode own rct stream");
        assert_eq!(img.components.len(), 3);
        for (ci, (comp, plane)) in img.components.iter().zip(planes.iter()).enumerate() {
            let got: Vec<u8> = comp.samples.iter().map(|&s| s as u8).collect();
            assert_eq!(&got[..], &plane[..], "comp {ci} samples (RCT)");
        }
        stream.len()
    }

    #[test]
    fn lossless_rct_round_trips() {
        let r = gradient(40, 32);
        let g = gradient(40, 32);
        let b = noise(40, 32, 0x0F0F_F0F0);
        roundtrip_rct(&[&r, &g, &b], 40, 32, 2, (4, 4));
    }

    #[test]
    fn lossless_rct_odd_dims_extremes() {
        // Saturated channels + odd dims: exercises the widened chroma
        // budget (QCC RI + 1) and the RCT corner values.
        let w = 19u32;
        let h = 27u32;
        let r = vec![255u8; (w * h) as usize];
        let g = vec![0u8; (w * h) as usize];
        let b: Vec<u8> = (0..w * h).map(|i| (i % 256) as u8).collect();
        roundtrip_rct(&[&r, &g, &b], w, h, 3, (4, 4));
    }

    #[test]
    fn rct_beats_independent_planes_on_correlated_rgb() {
        // A natural-ish correlated image: all three channels share the
        // same busy luminance (noise) while the channel *differences*
        // are smooth. The RCT moves the noise into one luma component
        // and leaves two near-flat chroma planes, so the MCT = 1 stream
        // must be smaller than the three-independent-planes stream.
        let w = 64u32;
        let h = 64u32;
        let luma = noise(w, h, 0x1122_3344);
        let r: Vec<u8> = luma.iter().map(|&v| v.saturating_add(10)).collect();
        let g = luma.clone();
        let b: Vec<u8> = luma.iter().map(|&v| v.saturating_sub(30)).collect();
        let rct_len = roundtrip_rct(&[&r, &g, &b], w, h, 3, (5, 5));
        let plain = encode_j2k_lossless(&[&r, &g, &b], w, h, 3, (5, 5)).unwrap();
        assert!(
            rct_len < plain.len(),
            "RCT stream ({rct_len} B) should beat independent planes ({} B)",
            plain.len()
        );
    }

    // -- 9-7 lossy path (Annex E scalar-expounded quantisation) --------

    /// Encode lossy, decode with this crate's decoder, and return the
    /// maximum absolute per-sample error plus the stream length.
    fn lossy_roundtrip(
        planes: &[&[u8]],
        w: u32,
        h: u32,
        nl: u8,
        cb: (u8, u8),
        fine_bits: u8,
    ) -> (u32, usize) {
        let stream = encode_j2k_lossy(planes, w, h, nl, cb, fine_bits).expect("encode lossy");
        let img = decode_j2k(&stream).expect("decode own lossy stream");
        assert_eq!(img.components.len(), planes.len());
        let mut max_err = 0u32;
        for (comp, plane) in img.components.iter().zip(planes) {
            for (&got, &want) in comp.samples.iter().zip(plane.iter()) {
                let err = (got - i32::from(want)).unsigned_abs();
                max_err = max_err.max(err);
            }
        }
        (max_err, stream.len())
    }

    #[test]
    fn lossy_9x7_near_lossless_within_one() {
        // fine_bits = 6 (Δb = 1/64): the quantisation error is far below
        // one sample step, so the decoded plane is within ±1 everywhere.
        let p = gradient(48, 40);
        let (max_err, _) = lossy_roundtrip(&[&p], 48, 40, 3, (4, 4), 6);
        assert!(max_err <= 1, "near-lossless error {max_err} > 1");
    }

    #[test]
    fn lossy_9x7_noise_bounded_error() {
        // Noise at fine_bits = 6 stays within ±1 too (the 9-7 float
        // pipeline is exact to well below half a step at Δb = 1/64).
        let p = noise(33, 27, 0x7777_AAAA);
        let (max_err, _) = lossy_roundtrip(&[&p], 33, 27, 2, (4, 4), 6);
        assert!(max_err <= 1, "noise error {max_err} > 1");
    }

    #[test]
    fn lossy_9x7_coarse_step_compresses_harder() {
        // Δb = 1 (fine_bits = 0) is a coarse quantiser: the stream must
        // be much smaller than the near-lossless one and the error still
        // modest (a few sample steps).
        let p = noise(64, 64, 0x0DDB_A115);
        let (err_fine, len_fine) = lossy_roundtrip(&[&p], 64, 64, 2, (5, 5), 6);
        let (err_coarse, len_coarse) = lossy_roundtrip(&[&p], 64, 64, 2, (5, 5), 0);
        assert!(err_fine <= 1);
        assert!(
            len_coarse < len_fine,
            "coarse ({len_coarse} B) should be smaller than fine ({len_fine} B)"
        );
        assert!(
            err_coarse <= 8,
            "coarse-step error {err_coarse} out of expected range"
        );
    }

    #[test]
    fn lossy_9x7_rgb_components() {
        let r = gradient(25, 31);
        let g = noise(25, 31, 0x5151_5151);
        let b = vec![64u8; 25 * 31];
        let (max_err, _) = lossy_roundtrip(&[&r, &g, &b], 25, 31, 2, (4, 4), 6);
        assert!(max_err <= 1, "rgb lossy error {max_err} > 1");
    }

    #[test]
    fn lossy_rejects_out_of_range_fine_bits() {
        let p = vec![0u8; 16];
        assert!(encode_j2k_lossy(&[&p], 4, 4, 1, (4, 4), 9).is_err());
    }

    // -- §B.12.1 progression orders on encode ---------------------------

    /// Encode `planes` in the given progression order (lossless 5-3),
    /// decode with this crate's decoder, assert bit-exact recovery and
    /// the signalled SGcod progression, and return the stream.
    fn roundtrip_order(
        planes: &[&[u8]],
        w: u32,
        h: u32,
        nl: u8,
        order: crate::ProgressionOrder,
    ) -> Vec<u8> {
        let stream = encode_j2k(
            planes,
            w,
            h,
            &EncodeParams {
                decomposition_levels: nl,
                code_block_exp: (4, 4),
                progression: order,
                ..EncodeParams::default()
            },
        )
        .expect("encode");
        let header = crate::parse_j2k_header(&stream).expect("own header parses");
        assert_eq!(header.cod.progression, order, "SGcod progression");
        let img = decode_j2k(&stream).expect("decode own stream");
        for (ci, (comp, plane)) in img.components.iter().zip(planes).enumerate() {
            let got: Vec<u8> = comp.samples.iter().map(|&s| s as u8).collect();
            assert_eq!(&got[..], &plane[..], "comp {ci} samples ({order:?})");
        }
        stream
    }

    #[test]
    fn all_five_progression_orders_round_trip() {
        // Multi-resolution RGB: with several resolution levels and three
        // components every §B.12.1 order produces a distinct packet
        // sequence, and each must decode bit-exactly.
        let w = 40u32;
        let h = 28u32;
        let r = gradient(w, h);
        let g = noise(w, h, 0x1357_9BDF);
        let b: Vec<u8> = gradient(w, h).iter().map(|&v| 255 - v).collect();
        let planes: [&[u8]; 3] = [&r, &g, &b];
        use crate::ProgressionOrder::*;
        let streams: Vec<Vec<u8>> = [Lrcp, Rlcp, Rpcl, Pcrl, Cprl]
            .into_iter()
            .map(|o| roundtrip_order(&planes, w, h, 3, o))
            .collect();
        // All five orders carry the same packets, merely reordered: the
        // stream lengths must agree.
        for i in 0..streams.len() {
            for j in (i + 1)..streams.len() {
                assert_eq!(
                    streams[i].len(),
                    streams[j].len(),
                    "same packets, reordered"
                );
            }
        }
        // With one layer and one precinct per resolution the
        // resolution-major orders (LRCP / RLCP / RPCL) coincide, but the
        // §B.12.1.4–5 position/component-major orders walk the packets
        // component-first — PCRL must differ from LRCP past the COD.
        assert_ne!(streams[0][60..], streams[3][60..], "PCRL reorders packets");
    }

    #[test]
    fn progression_orders_round_trip_odd_dims_gray() {
        // Odd dimensions push distinct trx0/try0 anchors into the
        // position-keyed corner projection.
        let p = noise(37, 23, 0xACE1_ACE1);
        for o in [
            crate::ProgressionOrder::Rpcl,
            crate::ProgressionOrder::Pcrl,
            crate::ProgressionOrder::Cprl,
        ] {
            roundtrip_order(&[&p], 37, 23, 2, o);
        }
    }

    #[test]
    fn reserved_progression_is_rejected() {
        let p = vec![0u8; 16];
        let r = encode_j2k(
            &[&p],
            4,
            4,
            &EncodeParams {
                decomposition_levels: 1,
                code_block_exp: (4, 4),
                progression: crate::ProgressionOrder::Reserved(9),
                ..EncodeParams::default()
            },
        );
        assert!(r.is_err());
    }

    // -- §B.6 user-defined precinct partitions on encode ---------------

    /// Encode with `params`, decode with this crate's decoder, assert
    /// bit-exact recovery, and return the stream.
    fn roundtrip_params(planes: &[&[u8]], w: u32, h: u32, params: &EncodeParams) -> Vec<u8> {
        let stream = encode_j2k(planes, w, h, params).expect("encode");
        let img = decode_j2k(&stream).expect("decode own stream");
        for (ci, (comp, plane)) in img.components.iter().zip(planes).enumerate() {
            let got: Vec<u8> = comp.samples.iter().map(|&s| s as u8).collect();
            assert_eq!(&got[..], &plane[..], "comp {ci} samples");
        }
        stream
    }

    #[test]
    fn multi_precinct_lossless_round_trips() {
        // 64×48, NL = 2, PP = (2, 3, 3): r = 0 partitions the 16×12 LL
        // domain into 4×4 cells (4×3 = 12 precincts) and the higher
        // levels split into effective 2^(3−1) = 4-sample precinct spans,
        // so every resolution carries several packets.
        let p = noise(64, 48, 0xFACE_FEED);
        let params = EncodeParams {
            decomposition_levels: 2,
            code_block_exp: (2, 2),
            precincts: vec![0x22, 0x33, 0x33],
            ..EncodeParams::default()
        };
        let stream = roundtrip_params(&[&p], 64, 48, &params);
        let header = crate::parse_j2k_header(&stream).expect("header");
        assert!(header.cod.user_defined_precincts);
        assert_eq!(header.cod.precincts, vec![0x22, 0x33, 0x33]);
    }

    #[test]
    fn multi_precinct_position_orders_round_trip() {
        // With several precincts per resolution the position-keyed
        // orders genuinely interleave (resolution, component, corner):
        // every order must still decode bit-exactly, and RPCL must now
        // produce a different packet sequence from LRCP.
        let w = 56u32;
        let h = 40u32;
        let r = gradient(w, h);
        let g = noise(w, h, 0x00C0_FFEE);
        let b: Vec<u8> = gradient(w, h).iter().map(|&v| v ^ 0x5A).collect();
        let planes: [&[u8]; 3] = [&r, &g, &b];
        use crate::ProgressionOrder::*;
        let mut streams = Vec::new();
        for o in [Lrcp, Rlcp, Rpcl, Pcrl, Cprl] {
            let params = EncodeParams {
                decomposition_levels: 2,
                code_block_exp: (2, 2),
                progression: o,
                precincts: vec![0x22, 0x33, 0x44],
                ..EncodeParams::default()
            };
            streams.push(roundtrip_params(&planes, w, h, &params));
        }
        for s in &streams[1..] {
            assert_eq!(streams[0].len(), s.len(), "same packets, reordered");
        }
        assert_ne!(
            streams[0][70..],
            streams[2][70..],
            "RPCL reorders multi-precinct packets"
        );
    }

    #[test]
    fn multi_precinct_lossy_round_trips() {
        // The 9-7 path over a multi-precinct partition.
        let p = noise(48, 48, 0x0BAD_CAFE);
        let params = EncodeParams {
            decomposition_levels: 2,
            code_block_exp: (3, 3),
            kernel: EncodeKernel::Lossy9x7 { fine_bits: 6 },
            precincts: vec![0x33, 0x44, 0x44],
            ..EncodeParams::default()
        };
        let stream = encode_j2k(&[&p], 48, 48, &params).expect("encode");
        let img = decode_j2k(&stream).expect("decode");
        let max_err = img.components[0]
            .samples
            .iter()
            .zip(p.iter())
            .map(|(&got, &want)| (got - i32::from(want)).unsigned_abs())
            .max()
            .unwrap();
        assert!(max_err <= 1, "multi-precinct lossy error {max_err} > 1");
    }

    #[test]
    fn precinct_validation_rejects_malformed() {
        let p = vec![0u8; 16 * 16];
        // Wrong byte count (NL + 1 = 3 required).
        let bad_len = EncodeParams {
            decomposition_levels: 2,
            code_block_exp: (2, 2),
            precincts: vec![0x33, 0x33],
            ..EncodeParams::default()
        };
        assert!(encode_j2k(&[&p], 16, 16, &bad_len).is_err());
        // Zero PPx nibble above r = 0 (Table A.21 note).
        let bad_zero = EncodeParams {
            decomposition_levels: 1,
            code_block_exp: (2, 2),
            precincts: vec![0x33, 0x30],
            ..EncodeParams::default()
        };
        assert!(encode_j2k(&[&p], 16, 16, &bad_zero).is_err());
        // A zero nibble at r = 0 alone is fine.
        let ok_zero = EncodeParams {
            decomposition_levels: 1,
            code_block_exp: (2, 2),
            precincts: vec![0x00, 0x33],
            ..EncodeParams::default()
        };
        assert!(encode_j2k(&[&p], 16, 16, &ok_zero).is_ok());
    }

    // -- §B.10 quality layers on encode (J.13.2-guided split) -----------

    #[test]
    fn multi_layer_lossless_round_trips() {
        // 3 layers over noisy content: every code-block's passes split
        // across layers at the J.13.4 truncation rates; decoding all
        // layers must remain bit-exact.
        let p = noise(64, 48, 0xD1CE_D1CE);
        let params = EncodeParams {
            decomposition_levels: 2,
            code_block_exp: (4, 4),
            layers: 3,
            ..EncodeParams::default()
        };
        let stream = roundtrip_params(&[&p], 64, 48, &params);
        let header = crate::parse_j2k_header(&stream).expect("header");
        assert_eq!(header.cod.layers, 3);
    }

    #[test]
    fn multi_layer_lossy_round_trips() {
        let p = noise(48, 40, 0xFEED_F00D);
        let params = EncodeParams {
            decomposition_levels: 2,
            code_block_exp: (4, 4),
            kernel: EncodeKernel::Lossy9x7 { fine_bits: 6 },
            layers: 4,
            ..EncodeParams::default()
        };
        let stream = encode_j2k(&[&p], 48, 40, &params).expect("encode");
        let img = decode_j2k(&stream).expect("decode");
        let max_err = img.components[0]
            .samples
            .iter()
            .zip(p.iter())
            .map(|(&got, &want)| (got - i32::from(want)).unsigned_abs())
            .max()
            .unwrap();
        assert!(max_err <= 1, "multi-layer lossy error {max_err} > 1");
    }

    #[test]
    fn multi_layer_multi_precinct_position_orders() {
        // Layers × precincts × the position-keyed orders: the §B.12.1
        // drivers interleave layers into the sweep and the per-precinct
        // tag-tree state must persist across the layer packets.
        let w = 48u32;
        let h = 32u32;
        let r = gradient(w, h);
        let g = noise(w, h, 0x5EED_5EED);
        let b: Vec<u8> = gradient(w, h).iter().map(|&v| v ^ 0xA5).collect();
        let planes: [&[u8]; 3] = [&r, &g, &b];
        for o in [
            crate::ProgressionOrder::Lrcp,
            crate::ProgressionOrder::Rpcl,
            crate::ProgressionOrder::Cprl,
        ] {
            let params = EncodeParams {
                decomposition_levels: 2,
                code_block_exp: (2, 2),
                progression: o,
                precincts: vec![0x22, 0x33, 0x44],
                layers: 3,
                ..EncodeParams::default()
            };
            roundtrip_params(&planes, w, h, &params);
        }
    }

    #[test]
    fn more_layers_than_coded_depths() {
        // A smooth gradient codes few bit-planes; with 8 layers several
        // layers receive no passes anywhere (empty packets) and blocks
        // skip layers between contributions.
        let p = gradient(32, 32);
        let params = EncodeParams {
            decomposition_levels: 1,
            code_block_exp: (4, 4),
            layers: 8,
            ..EncodeParams::default()
        };
        roundtrip_params(&[&p], 32, 32, &params);
    }

    #[test]
    fn multi_layer_flat_image_all_empty() {
        // Flat mid-grey: every packet of every layer is empty.
        let p = vec![128u8; 24 * 24];
        let params = EncodeParams {
            decomposition_levels: 2,
            code_block_exp: (4, 4),
            layers: 4,
            ..EncodeParams::default()
        };
        roundtrip_params(&[&p], 24, 24, &params);
    }

    #[test]
    fn multi_layer_overhead_is_modest() {
        // Splitting into layers adds only packet-header overhead: the
        // 4-layer stream must stay within a few percent (plus a fixed
        // floor) of the single-layer stream.
        let p = noise(64, 64, 0x1234_4321);
        let single = encode_j2k(
            &[&p],
            64,
            64,
            &EncodeParams {
                decomposition_levels: 2,
                code_block_exp: (4, 4),
                ..EncodeParams::default()
            },
        )
        .unwrap();
        let layered = encode_j2k(
            &[&p],
            64,
            64,
            &EncodeParams {
                decomposition_levels: 2,
                code_block_exp: (4, 4),
                layers: 4,
                ..EncodeParams::default()
            },
        )
        .unwrap();
        assert!(layered.len() > single.len(), "layer headers cost bytes");
        assert!(
            layered.len() < single.len() + single.len() / 10 + 256,
            "4-layer overhead too large: {} vs {}",
            layered.len(),
            single.len()
        );
    }

    #[test]
    fn zero_layers_rejected() {
        let p = vec![0u8; 16];
        let params = EncodeParams {
            decomposition_levels: 1,
            code_block_exp: (2, 2),
            layers: 0,
            ..EncodeParams::default()
        };
        assert!(encode_j2k(&[&p], 4, 4, &params).is_err());
    }

    // -- Annex J.13.3 PCRD rate control ---------------------------------

    fn mse_of(stream: &[u8], plane: &[u8]) -> f64 {
        let img = decode_j2k(stream).expect("decode rate-controlled stream");
        let mut acc = 0.0f64;
        for (&got, &want) in img.components[0].samples.iter().zip(plane.iter()) {
            let e = f64::from(got - i32::from(want));
            acc += e * e;
        }
        acc / plane.len() as f64
    }

    #[test]
    fn rate_control_meets_budget_and_uses_it() {
        let p = noise(64, 64, 0x7A57_7A57);
        let base = EncodeParams {
            decomposition_levels: 2,
            code_block_exp: (4, 4),
            ..EncodeParams::default()
        };
        let full = encode_j2k(&[&p], 64, 64, &base).unwrap();
        let target = full.len() * 6 / 10;
        let rc = encode_j2k(
            &[&p],
            64,
            64,
            &EncodeParams {
                target_bytes: Some(target),
                ..base
            },
        )
        .unwrap();
        assert!(rc.len() <= target, "budget {target}, got {}", rc.len());
        // J.13.3: the residual gap to the budget is small.
        assert!(
            rc.len() + 150 >= target,
            "budget {target} under-used: {}",
            rc.len()
        );
    }

    #[test]
    fn rate_control_quality_monotone_in_budget() {
        let p = noise(64, 64, 0x0DD5_0DD5);
        let base = EncodeParams {
            decomposition_levels: 2,
            code_block_exp: (4, 4),
            ..EncodeParams::default()
        };
        let full_len = encode_j2k(&[&p], 64, 64, &base).unwrap().len();
        let mut last_mse = f64::INFINITY;
        for frac in [3usize, 5, 8] {
            let target = full_len * frac / 10;
            let rc = encode_j2k(
                &[&p],
                64,
                64,
                &EncodeParams {
                    target_bytes: Some(target),
                    ..base.clone()
                },
            )
            .unwrap();
            assert!(rc.len() <= target);
            let mse = mse_of(&rc, &p);
            assert!(
                mse <= last_mse,
                "MSE must not increase with budget: {mse} > {last_mse}"
            );
            last_mse = mse;
        }
        // At 80% of lossless the truncation error must be mild.
        assert!(last_mse < 60.0, "80%-budget MSE too large: {last_mse}");
    }

    #[test]
    fn rate_control_generous_budget_stays_lossless() {
        let p = gradient(48, 32);
        let base = EncodeParams {
            decomposition_levels: 2,
            code_block_exp: (4, 4),
            ..EncodeParams::default()
        };
        let full = encode_j2k(&[&p], 48, 32, &base).unwrap();
        let rc = encode_j2k(
            &[&p],
            48,
            32,
            &EncodeParams {
                target_bytes: Some(full.len() + 100),
                ..base
            },
        )
        .unwrap();
        assert_eq!(rc, full, "roomy budget must not truncate");
        let img = decode_j2k(&rc).expect("decode");
        let got: Vec<u8> = img.components[0].samples.iter().map(|&s| s as u8).collect();
        assert_eq!(got, p);
    }

    #[test]
    fn rate_control_tiny_budget_yields_minimal_stream() {
        // A budget below the marker + empty-packet floor: the encoder
        // returns the smallest legal stream (everything truncated away)
        // and it still decodes (to a flat plane).
        let p = noise(32, 32, 0xBEE5_BEE5);
        let rc = encode_j2k(
            &[&p],
            32,
            32,
            &EncodeParams {
                decomposition_levels: 2,
                code_block_exp: (4, 4),
                target_bytes: Some(30),
                ..EncodeParams::default()
            },
        )
        .unwrap();
        assert!(rc.len() < 120, "minimal stream unexpectedly large");
        let img = decode_j2k(&rc).expect("minimal stream decodes");
        assert_eq!(img.components[0].samples.len(), 32 * 32);
    }

    #[test]
    fn rate_control_composes_with_layers_and_97() {
        let p = noise(64, 48, 0xCAB1_CAB1);
        let base = EncodeParams {
            decomposition_levels: 2,
            code_block_exp: (4, 4),
            kernel: EncodeKernel::Lossy9x7 { fine_bits: 6 },
            layers: 3,
            ..EncodeParams::default()
        };
        let full_len = encode_j2k(&[&p], 64, 48, &base).unwrap().len();
        let target = full_len * 7 / 10;
        let rc = encode_j2k(
            &[&p],
            64,
            48,
            &EncodeParams {
                target_bytes: Some(target),
                ..base
            },
        )
        .unwrap();
        assert!(rc.len() <= target);
        let header = crate::parse_j2k_header(&rc).expect("header");
        assert_eq!(header.cod.layers, 3);
        let mse = mse_of(&rc, &p);
        assert!(mse < 100.0, "layered rate-controlled MSE too large: {mse}");
    }

    // -- §G.3.1 irreversible component transform (MCT = 1, 9-7) --------

    /// Encode three planes lossy with the ICT, decode with this crate's
    /// decoder, and return (max abs error, stream length).
    fn lossy_ict_roundtrip(
        planes: &[&[u8]; 3],
        w: u32,
        h: u32,
        nl: u8,
        cb: (u8, u8),
        fine_bits: u8,
    ) -> (u32, usize) {
        let stream = encode_j2k_lossy_ict(planes, w, h, nl, cb, fine_bits).expect("encode ict");
        let img = decode_j2k(&stream).expect("decode own ict stream");
        assert_eq!(img.components.len(), 3);
        let mut max_err = 0u32;
        for (comp, plane) in img.components.iter().zip(planes.iter()) {
            for (&got, &want) in comp.samples.iter().zip(plane.iter()) {
                max_err = max_err.max((got - i32::from(want)).unsigned_abs());
            }
        }
        (max_err, stream.len())
    }

    #[test]
    fn lossy_ict_near_lossless_within_one() {
        // fine_bits = 6 (Δb = 1/64): the ICT rows have bounded gain, so
        // the composed §G.3.1 → 9-7 → §G.3.2 error stays within ±1.
        let r = gradient(48, 40);
        let g = noise(48, 40, 0x2468_ACE0);
        let b: Vec<u8> = gradient(48, 40).iter().map(|&v| 255 - v).collect();
        let (max_err, _) = lossy_ict_roundtrip(&[&r, &g, &b], 48, 40, 3, (4, 4), 6);
        assert!(max_err <= 1, "ict near-lossless error {max_err} > 1");
    }

    #[test]
    fn lossy_ict_odd_dims_extremes() {
        // Saturated channels + odd dims: the G-9..G-11 corner values
        // (Y1 / Y2 at ±half range) with the PSEO parity paths.
        let w = 19u32;
        let h = 27u32;
        let r = vec![255u8; (w * h) as usize];
        let g = vec![0u8; (w * h) as usize];
        let b: Vec<u8> = (0..w * h).map(|i| (i % 256) as u8).collect();
        let (max_err, _) = lossy_ict_roundtrip(&[&r, &g, &b], w, h, 2, (4, 4), 6);
        assert!(max_err <= 1, "ict extremes error {max_err} > 1");
    }

    #[test]
    fn ict_beats_independent_planes_on_correlated_rgb() {
        // Shared busy luminance, smooth channel differences: the ICT
        // concentrates the noise into Y0 and leaves near-flat chroma,
        // so the MCT = 1 stream must beat three independent planes at
        // the same quantisation step.
        let w = 64u32;
        let h = 64u32;
        let luma = noise(w, h, 0x9E37_79B9);
        let r: Vec<u8> = luma.iter().map(|&v| v.saturating_add(12)).collect();
        let g = luma.clone();
        let b: Vec<u8> = luma.iter().map(|&v| v.saturating_sub(25)).collect();
        let (err, ict_len) = lossy_ict_roundtrip(&[&r, &g, &b], w, h, 3, (5, 5), 4);
        assert!(err <= 2, "ict correlated error {err} > 2");
        let plain = encode_j2k_lossy(&[&r, &g, &b], w, h, 3, (5, 5), 4).unwrap();
        assert!(
            ict_len < plain.len(),
            "ICT stream ({ict_len} B) should beat independent planes ({} B)",
            plain.len()
        );
    }

    #[test]
    fn ict_signals_mct_and_9x7() {
        // Wire shape: SGcod MCT = 1 and the Table A.20 9-7 kernel byte.
        let r = gradient(16, 16);
        let g = gradient(16, 16);
        let b = gradient(16, 16);
        let stream = encode_j2k_lossy_ict(&[&r, &g, &b], 16, 16, 1, (4, 4), 6).unwrap();
        let header = crate::parse_j2k_header(&stream).expect("own header parses");
        assert_eq!(header.cod.multi_component_transform, 1);
        assert_eq!(
            header.cod.transform,
            crate::WaveletTransform::Irreversible9x7
        );
    }
}
