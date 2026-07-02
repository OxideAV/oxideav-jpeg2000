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
//! reference-grid origin, one quality layer, LRCP progression, maximum
//! precincts (`PPx = PPy = 15`), no MCT, the reversible 5-3 kernel, and
//! the "no quantization" style (T.800 Table A.28 style 0) — i.e. fully
//! **lossless**: decoding reproduces the input samples bit-exactly.
//!
//! The geometry (tile / resolution-level / precinct / code-block
//! enumeration) is obtained from the same [`crate::geometry`] functions
//! the decoder uses, so the encoder's packet layout agrees with the
//! decoder's walk by construction; packets are emitted in the order
//! [`crate::progression::lrcp_packet_order`] enumerates them.
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
use crate::progression::{lrcp_packet_order, ComponentProgressionInfo};
use crate::t1::{reset_contexts, CodeBlock, Coefficient};
use crate::{
    Error, Siz, SizComponent, MARKER_COD, MARKER_EOC, MARKER_QCD, MARKER_SIZ, MARKER_SOC,
    MARKER_SOD, MARKER_SOT,
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

/// One tier-1-encoded code-block ready for packet assembly.
struct EncodedBlock {
    /// §B.10.5 zero-bit-plane count `P = Mb − planes`.
    zero_bit_planes: u32,
    /// §B.10.6 coding passes (`3 · planes − 2`), zero for an all-zero
    /// (not included) block.
    coding_passes: u32,
    /// The single §C.3 codeword segment.
    bytes: Vec<u8>,
}

/// Tier-1 encode one code-block's coefficients through the full §D.3
/// schedule into one codeword segment. `mb` is the Equation E-2 budget
/// for the block's sub-band. Returns `None` for an all-zero block (not
/// included in any packet).
fn encode_code_block(
    orientation: SubBandOrientation,
    width: usize,
    height: usize,
    targets: &[Coefficient],
    mb: u32,
) -> Result<Option<EncodedBlock>, Error> {
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
    let mut enc_block = CodeBlock::new(orientation, width, height);
    let mut encoder = MqEncoder::new();
    let mut ctx = reset_contexts();
    enc_block.cleanup_encode(top, targets, &mut encoder, &mut ctx);
    for p in (0..top).rev() {
        enc_block.significance_propagation_encode(p, targets, &mut encoder, &mut ctx);
        enc_block.magnitude_refinement_encode(p, targets, &mut encoder, &mut ctx);
        enc_block.cleanup_encode(p, targets, &mut encoder, &mut ctx);
    }
    Ok(Some(EncodedBlock {
        zero_bit_planes: mb - planes,
        coding_passes: 3 * planes - 2,
        bytes: encoder.flush(),
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
    encode_impl(planes, width, height, nl, cb_exp, false)
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
    encode_impl(planes.as_slice(), width, height, nl, cb_exp, true)
}

fn encode_impl(
    planes: &[&[u8]],
    width: u32,
    height: u32,
    nl: u8,
    cb_exp: (u8, u8),
    use_rct: bool,
) -> Result<Vec<u8>, Error> {
    let (xcb, ycb) = cb_exp;
    debug_assert!(!use_rct || planes.len() == 3);
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
    // §G.1.2 DC level shift, then (optionally) the §G.2 forward RCT
    // across components 0–2, then the per-component §F.4 cascade.
    let dc = 1i32 << (PRECISION - 1);
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
    let bands: Vec<ComponentBands> = shifted
        .into_iter()
        .map(|p| forward_cascade(p, width as usize, height as usize, nl))
        .collect();

    // -- Geometry (shared with the decoder) ---------------------------
    let tile = derive_tile_geometry(&siz, 0)?;
    let levels_per_comp: Vec<_> = tile
        .components
        .iter()
        .map(|tc| derive_resolution_levels(*tc, nl))
        .collect();

    // -- Tier-1: encode every code-block, keyed (comp, r, precinct) ---
    // Per packet (comp, r, k): the per-sub-band encoder plans and the
    // per-block plan + body bytes in §B.10.8 order.
    struct PacketData {
        sub_band_plans: Vec<SubBandEncoderPlan>,
        plans: Vec<CodeBlockPlan>,
        body: Vec<u8>,
    }
    use std::collections::BTreeMap;
    let mut packets: BTreeMap<(u16, u8, u32), PacketData> = BTreeMap::new();
    let mut precincts_per_comp_res: Vec<Vec<u32>> = Vec::with_capacity(planes.len());

    for (ci, levels) in levels_per_comp.iter().enumerate() {
        let mut precincts_at_r = Vec::with_capacity(levels.len());
        for level in levels {
            let pp = precinct_exponents_at(&[], level.r);
            let partition = derive_precinct_partition(level, pp);
            let num_precincts = partition.num_precincts() as u32;
            precincts_at_r.push(num_precincts);
            for k in 0..num_precincts {
                let pcb = derive_precinct_code_blocks(level, pp, xcb, ycb, k)?;
                let mut sub_band_plans: Vec<SubBandEncoderPlan> = Vec::new();
                let mut plans: Vec<CodeBlockPlan> = Vec::new();
                let mut body: Vec<u8> = Vec::new();
                for psb in &pcb.sub_bands {
                    let geom = SubBandGeometry {
                        width: psb.grid_wide,
                        height: psb.grid_high,
                    };
                    let nblocks = (psb.grid_wide as usize) * (psb.grid_high as usize);
                    let mut first_layer = vec![1u32; nblocks]; // 1 = never (single layer)
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
                    let eps = ri + u32::from(band_gain(psb.orientation));
                    let mb = u32::from(GUARD_BITS) + eps - 1;
                    for (bi, cb) in psb.code_blocks.iter().enumerate() {
                        let (bw, bh) = (cb.width() as usize, cb.height() as usize);
                        if bw == 0 || bh == 0 {
                            plans.push(CodeBlockPlan {
                                included: false,
                                zero_bit_planes: 0,
                                coding_passes: 0,
                                segment_length: 0,
                            });
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
                        match encode_code_block(psb.orientation, bw, bh, &targets, mb)? {
                            Some(enc) => {
                                first_layer[bi] = 0;
                                zbp[bi] = enc.zero_bit_planes;
                                plans.push(CodeBlockPlan {
                                    included: true,
                                    zero_bit_planes: enc.zero_bit_planes,
                                    coding_passes: enc.coding_passes,
                                    segment_length: enc.bytes.len() as u32,
                                });
                                body.extend_from_slice(&enc.bytes);
                            }
                            None => {
                                plans.push(CodeBlockPlan {
                                    included: false,
                                    zero_bit_planes: 0,
                                    coding_passes: 0,
                                    segment_length: 0,
                                });
                            }
                        }
                    }
                    sub_band_plans.push((geom, first_layer, zbp));
                }
                packets.insert(
                    (ci as u16, level.r, k),
                    PacketData {
                        sub_band_plans,
                        plans,
                        body,
                    },
                );
            }
        }
        precincts_per_comp_res.push(precincts_at_r);
    }

    // -- Tier-2: emit packets in LRCP order ----------------------------
    let prog_info: Vec<ComponentProgressionInfo> = precincts_per_comp_res
        .iter()
        .map(|pr| ComponentProgressionInfo {
            num_decomposition_levels: nl,
            precincts_per_resolution: pr.clone(),
        })
        .collect();
    let order = lrcp_packet_order(1, &prog_info)?;
    let mut tile_body: Vec<u8> = Vec::new();
    for desc in &order {
        let pd = packets
            .get(&(desc.component, desc.resolution, desc.precinct))
            .ok_or(Error::InvalidPacketHeader)?;
        // Single layer → fresh per-precinct encoder state per packet.
        let mut state = PrecinctEncoderState::new(&pd.sub_band_plans);
        let header = encode_packet_header(&mut state, 0, &pd.plans);
        tile_body.extend_from_slice(&header);
        tile_body.extend_from_slice(&pd.body);
    }

    // -- Markers --------------------------------------------------------
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

    // COD (Tables A.13 – A.21): Scod = 0 (no precincts / SOP / EPH),
    // LRCP, 1 layer, MCT per `use_rct` (Table A.17), NL levels,
    // code-block exponents − 2, style 0, 5-3 reversible.
    let cod_payload = [
        0u8, // Scod
        0,   // SGcod: progression = LRCP
        0,
        1,             // SGcod: layers = 1
        use_rct as u8, // SGcod: MCT (Table A.17)
        nl,            // SPcod: NL
        xcb - 2,       // SPcod: xcb − 2
        ycb - 2,       // SPcod: ycb − 2
        0,             // SPcod: code-block style
        1,             // SPcod: transform = 5-3 reversible (Table A.20)
    ];
    push_segment(&mut out, MARKER_COD, &cod_payload);

    // QCD (Tables A.27 – A.28): style 0 (no quantization), G guard
    // bits; one εb byte per sub-band in the §F.3.1 order (NLLL then
    // per-level HL, LH, HH from the deepest level outward). `ri` is the
    // component bit depth the exponents build on.
    let quant_payload = |ri: u8| -> Vec<u8> {
        let mut p = Vec::with_capacity(2 + 3 * nl as usize);
        p.push(GUARD_BITS << 5); // Sqcd/Sqcc: style 0 | guard bits
        p.push(ri << 3); // εb(LL) = RI + 0
        for _r in 1..=nl {
            p.push((ri + 1) << 3); // HL: RI + 1
            p.push((ri + 1) << 3); // LH: RI + 1
            p.push((ri + 2) << 3); // HH: RI + 2
        }
        p
    };
    push_segment(&mut out, MARKER_QCD, &quant_payload(PRECISION));
    if use_rct {
        // §G.2 / §A.6.5: the RCT chrominance components (1, 2) carry one
        // extra bit of dynamic range — override their exponents with a
        // main-header QCC each (`Main QCC > Main QCD`). Cqcc is one byte
        // (Csiz = 3 < 257).
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
}
