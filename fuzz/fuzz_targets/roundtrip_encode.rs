#![no_main]

//! Structure-aware fuzz target for the **encoder**: build an
//! [`EncodeParams`] from the fuzz bytes, encode deterministic sample
//! planes, and require the crate's own decoder to accept the product —
//! `decode(encode(x))` must never fail on a stream this encoder
//! emitted, and on the reversible full-rate path it must reproduce
//! `x` **bit-exactly**.
//!
//! The parameter generator walks the whole `EncodeParams` surface:
//!
//! * both Table A.20 kernels (5-3 reversible / 9-7 with every
//!   `fine_bits` step) and the Table A.17 MCT pairings;
//! * all five §B.12.1 progression orders, §B.6 user precinct
//!   partitions, §A.6.6 full-coverage `POC` entries;
//! * quality layers, Annex J.13.3 PCRD byte budgets, multi-tile
//!   grids, §A.4.2 tile-part splits on every axis;
//! * all six Table A.19 Annex D styles — §D.6 bypass, context reset,
//!   §D.4.2 per-pass termination, §D.7 vertically causal contexts,
//!   predictable termination, §D.5 segmentation symbols — SOP / EPH
//!   framing, §A.7.4 / §A.7.5 PPM / PPT header relocation;
//! * SIZ component sub-sampling, per-component `COC` / `QCC`
//!   overrides (mixed kernels included), the Annex H Maxshift ROI;
//! * the T.814 lanes — HTONLY (`high_throughput`), the `Z_blk = 3`
//!   refinement shape (`ht_refinement`), MULTIHT quality layers, and
//!   the §8.2 / §A.4 MIXED set (`ht_mixed`).
//!
//! Illegal combinations must surface a clean `Err` from the encoder
//! (also fuzzed here); a successful encode is then decoded three ways
//! (full, `max_layers = 1`, and — where every component keeps the
//! uniform `NL ≥ 1` — `discard_levels = 1`) and any decode failure or
//! reversible round-trip mismatch panics the harness.
//!
//! ## Bounds
//!
//! Geometry is capped (≤ 48 × 48, ≤ 3 components, `NL ≤ 5`, ≤ 4
//! layers) so an iteration never allocates attacker-scaled memory and
//! the PCRD bisection stays cheap.

use libfuzzer_sys::fuzz_target;
use oxideav_jpeg2000::encode::{
    encode_j2k, ComponentOverride, EncodeKernel, EncodeParams, PackedHeaders, TilePartSplit,
};
use oxideav_jpeg2000::{
    decode_j2k, decode_j2k_layers, decode_j2k_reduced, PocProgression, ProgressionOrder,
};

/// Byte cursor over the fuzz input; exhausted reads yield `0` so every
/// prefix is a valid parameter script.
struct Cur<'a> {
    d: &'a [u8],
    i: usize,
}

impl Cur<'_> {
    fn u8(&mut self) -> u8 {
        let b = self.d.get(self.i).copied().unwrap_or(0);
        self.i += 1;
        b
    }

    fn bool(&mut self) -> bool {
        self.u8() & 1 != 0
    }
}

const ORDERS: [ProgressionOrder; 5] = [
    ProgressionOrder::Lrcp,
    ProgressionOrder::Rlcp,
    ProgressionOrder::Rpcl,
    ProgressionOrder::Pcrl,
    ProgressionOrder::Cprl,
];

fuzz_target!(|data: &[u8]| {
    let mut c = Cur { d: data, i: 0 };

    // --- Geometry -------------------------------------------------
    let width = 1 + u32::from(c.u8()) % 48;
    let height = 1 + u32::from(c.u8()) % 48;
    let ncomp = 1 + usize::from(c.u8()) % 3;

    // Optional SIZ sub-sampling (§B.2): factors 1..=4 per component.
    let sub_sampling: Vec<(u8, u8)> = if c.bool() {
        (0..ncomp).map(|_| (1 + c.u8() % 4, 1 + c.u8() % 4)).collect()
    } else {
        Vec::new()
    };

    // --- Coding lane ------------------------------------------------
    // 0..=3 Annex D (styles from bits), 4 HTONLY, 5 HT + refinement,
    // 6 MIXED, 7 HT MULTIHT (layers > 1).
    let mode = c.u8() % 8;
    let ht = mode >= 4;
    let mixed = mode == 6;

    let kernel = if c.bool() {
        EncodeKernel::Lossless5x3
    } else {
        EncodeKernel::Lossy9x7 { fine_bits: c.u8() % 6 }
    };

    let nl = c.u8() % 6;
    let xcb = 2 + c.u8() % 4;
    let ycb = 2 + c.u8() % 4;
    let mct = ncomp == 3 && c.bool();
    let progression = ORDERS[usize::from(c.u8()) % 5];

    let layers: u16 = if mixed {
        1
    } else if mode == 7 {
        2 + u16::from(c.u8() % 3)
    } else {
        1 + u16::from(c.u8() % 4)
    };

    // §B.6 user precinct partition: one Table A.21 byte per r = 0..=NL
    // (a 0 nibble only at r = 0).
    let precincts: Vec<u8> = if c.bool() {
        (0..=nl)
            .map(|r| {
                let lo = if r == 0 { c.u8() % 9 } else { 1 + c.u8() % 8 };
                let hi = if r == 0 { c.u8() % 9 } else { 1 + c.u8() % 8 };
                lo | (hi << 4)
            })
            .collect()
    } else {
        Vec::new()
    };

    let tile_size = if c.bool() {
        Some((4 + u32::from(c.u8()) % 44, 4 + u32::from(c.u8()) % 44))
    } else {
        None
    };

    // Annex-D-only style bits (rejected in the HT lanes by contract —
    // exercise both the accept and the clean-reject path).
    let bypass = c.bool() && (!ht || c.bool());
    let terminate_all = c.bool() && (!ht || c.bool());
    // Table A.19 bits 1 / 3 / 4 / 5 — the coder-shaping styles.
    let style_bits = c.u8();
    let reset_probabilities = style_bits & 1 != 0 && (!ht || c.bool());
    let vertically_causal = style_bits & 2 != 0 && (!ht || c.bool());
    let predictable_termination = style_bits & 4 != 0 && (!ht || c.bool());
    let segmentation_symbols = style_bits & 8 != 0 && (!ht || c.bool());

    let sop = c.bool();
    let eph = c.bool();

    let tile_parts = match c.u8() % 4 {
        0 => TilePartSplit::Single,
        1 => TilePartSplit::ByResolution,
        2 => TilePartSplit::ByLayer,
        _ => TilePartSplit::ByComponent,
    };

    let packed_headers = match c.u8() % 3 {
        0 => PackedHeaders::InStream,
        1 => PackedHeaders::Ppt,
        _ => PackedHeaders::Ppm,
    };

    // §A.6.6 POC: a single full-coverage volume in a (possibly)
    // different order, or none.
    let poc: Vec<PocProgression> = if c.bool() {
        vec![PocProgression {
            resolution_start: 0,
            component_start: 0,
            layer_end: layers,
            resolution_end: nl + 1,
            component_end: ncomp as u16,
            progression: ORDERS[usize::from(c.u8()) % 5],
        }]
    } else {
        Vec::new()
    };

    // Per-component COC / QCC override (§A.6.2 / §A.6.5). A kernel
    // override (mixed 5-3 / 9-7 siblings) only pairs with MCT off.
    let mut lossy_override = false;
    let component_overrides: Vec<ComponentOverride> = if c.bool() {
        let over_kernel = if !mct && c.bool() {
            let k = if c.bool() {
                EncodeKernel::Lossless5x3
            } else {
                lossy_override = true;
                EncodeKernel::Lossy9x7 { fine_bits: c.u8() % 6 }
            };
            Some(k)
        } else {
            None
        };
        vec![ComponentOverride {
            component: u16::from(c.u8()) % ncomp as u16,
            decomposition_levels: c.bool().then(|| c.u8() % 6),
            code_block_exp: c.bool().then(|| (2 + c.u8() % 4, 2 + c.u8() % 4)),
            precincts: c.bool().then(Vec::new),
            kernel: over_kernel,
        }]
    } else {
        Vec::new()
    };

    // Annex H ROI and the PCRD budget (both barred from MIXED).
    let roi = if !mixed && c.bool() {
        let x0 = u32::from(c.u8()) % width;
        let y0 = u32::from(c.u8()) % height;
        Some(oxideav_jpeg2000::encode::RoiRegion {
            x0,
            y0,
            x1: x0 + 1 + u32::from(c.u8()) % (width - x0),
            y1: y0 + 1 + u32::from(c.u8()) % (height - y0),
        })
    } else {
        None
    };
    let target_bytes = if !ht && c.bool() {
        Some(64 + usize::from(c.u8()) * 8)
    } else {
        None
    };

    let params = EncodeParams {
        decomposition_levels: nl,
        code_block_exp: (xcb, ycb),
        kernel,
        mct,
        progression,
        precincts,
        layers,
        target_bytes,
        tile_size,
        bypass,
        terminate_all,
        reset_probabilities,
        vertically_causal,
        predictable_termination,
        segmentation_symbols,
        sop,
        eph,
        sub_sampling: sub_sampling.clone(),
        tile_parts,
        poc,
        component_overrides: component_overrides.clone(),
        packed_headers,
        high_throughput: ht && !mixed,
        ht_refinement: mode == 5,
        ht_mixed: mixed,
        roi,
    };

    // --- Sample planes (deterministic from the remaining bytes) ----
    let planes_data: Vec<Vec<u8>> = (0..ncomp)
        .map(|comp| {
            let (xr, yr) = sub_sampling.get(comp).copied().unwrap_or((1, 1));
            let pw = width.div_ceil(u32::from(xr));
            let ph = height.div_ceil(u32::from(yr));
            let mut seed = c.u8();
            (0..pw * ph)
                .map(|k| {
                    seed = seed
                        .wrapping_mul(197)
                        .wrapping_add(c.u8())
                        .wrapping_add(k as u8);
                    seed
                })
                .collect()
        })
        .collect();
    let plane_refs: Vec<&[u8]> = planes_data.iter().map(Vec::as_slice).collect();

    let Ok(stream) = encode_j2k(&plane_refs, width, height, &params) else {
        // Illegal combination — the clean reject is the tested contract.
        return;
    };

    // A stream this encoder emitted must decode.
    let img = decode_j2k(&stream).expect("own encode must decode");
    assert_eq!(img.components.len(), ncomp, "component count survives");

    // Reversible full-rate path: bit-exact round trip.
    let reversible = matches!(kernel, EncodeKernel::Lossless5x3) && !lossy_override;
    if reversible && target_bytes.is_none() {
        for (comp, plane) in planes_data.iter().enumerate() {
            let dc = &img.components[comp];
            assert_eq!(
                (dc.width * dc.height) as usize,
                plane.len(),
                "plane geometry survives"
            );
            for (k, &px) in plane.iter().enumerate() {
                assert_eq!(dc.samples[k], i32::from(px), "reversible sample {k}");
            }
        }
    }

    // Layer-progressive decode of the first layer always exists.
    decode_j2k_layers(&stream, 1).expect("layer-1 decode of own encode");

    // Reduced-resolution decode where the uniform NL permits it.
    let uniform_nl = component_overrides
        .iter()
        .all(|o| o.decomposition_levels.is_none());
    if nl >= 1 && uniform_nl {
        decode_j2k_reduced(&stream, 1).expect("r1 decode of own encode");
    }
});
