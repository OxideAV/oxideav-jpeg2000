//! End-to-end T.800 codestream decode wiring.
//!
//! This module composes the per-Annex stages the crate has grown —
//! §A main-header + tile-part parsing ([`crate::parse_codestream`]),
//! §B.12 progression-order packet enumeration
//! ([`crate::progression`]), §B.10 packet-header decoding
//! ([`crate::packet::walk_packet_headers`]), §C/§D tier-1 MQ
//! coefficient decoding ([`crate::t1`]), Annex E sub-band reassembly
//! with the §F.3.1 inverse-DWT cascade ([`crate::reassemble`]), and
//! the Annex G inverse multiple-component transform with DC level
//! shift ([`crate::mct`]) — into one public entry point:
//! [`decode_j2k`].
//!
//! ## Coverage
//!
//! The wiring handles the T.800 baseline geometry classes:
//!
//! * any tile grid (each tile decoded independently per §B.3, with
//!   multiple tile-parts per tile concatenated in `TPsot` order),
//! * any decomposition-level count `NL ∈ 0..=32` and any precinct /
//!   code-block partition the §B.6 / §B.7 derivations admit,
//! * `LRCP` and `RLCP` progression orders (§B.12.1.1 / §B.12.1.2),
//!   single or multiple layers,
//! * both wavelet kernels: 5-3 reversible (quantisation style
//!   "none", Table A.28) and 9-7 irreversible (scalar-derived or
//!   scalar-expounded step sizes),
//! * `SGcod` MCT on/off — inverse RCT (§G.2.2) with the 5-3 kernel,
//!   inverse ICT (§G.3.2) with the 9-7 kernel, both with index-`≥ 3`
//!   component pass-through,
//! * per-component sub-sampling via `XRsiz` / `YRsiz` (each
//!   component plane is reconstructed on its own §B.2 component
//!   grid; no upsampling is performed),
//! * SOP / EPH packet framing per the COD `Scod` bits.
//!
//! Streams that need machinery this round does not wire are
//! **rejected** with [`Error::NotImplemented`] rather than
//! mis-decoded: `COC` / `QCC` per-component overrides, tile-part
//! header overrides (`COD` / `QCD` inside a tile-part), `RGN` ROI
//! shifts, `POC` progression-order changes, `PPM` / `PPT` packed
//! packet headers, the `RPCL` / `PCRL` / `CPRL` progression orders,
//! and the Table A.19 code-block-style bits that change codeword
//! segmentation (selective arithmetic-coding bypass, context reset,
//! termination on each coding pass).
//!
//! All behaviour is derived from the staged T.800 specification
//! text. The
//! committed test fixtures were produced and cross-checked with an
//! encoder/decoder binary invoked strictly as an opaque black box.

use std::collections::BTreeMap;

use crate::dequant::{self, StepSize};
use crate::geometry::{
    derive_precinct_code_blocks, derive_precinct_partition, derive_resolution_levels,
    derive_tile_geometry, image_area, precinct_exponents_at, tile_grid_extent, PrecinctCodeBlocks,
    ResolutionLevel, SubBand, SubBandOrientation,
};
use crate::mct::{
    reconstruct_tile_components_5x3_multi, reconstruct_tile_components_9x7_multi,
    ComponentDescriptor, InverseMctMode,
};
use crate::packet::{PacketGeometry, SegmentSplit, SopEphMode, SubBandGeometry};
use crate::progression::{
    cprl_packet_order, lrcp_packet_order, pcrl_packet_order, rlcp_packet_order, rpcl_packet_order,
    ComponentPositionInfo, ComponentProgressionInfo, ResolutionPrecinctLayout,
};
use crate::reassemble::{
    idwt_5x3, idwt_9x7, BlockSource, CodedCodeBlock, PrecinctBlocks, SubBandQuantization,
    WalkerBlockEntry, WalkerBlockSource,
};
use crate::t1::{reset_contexts, BitPlaneSequencer, CodeBlock};
use crate::{
    Error, J2kCodestream, ProgressionOrder, QuantizationStyle, Siz, TilePartMarker,
    WaveletTransform, MARKER_CAP, MARKER_COC, MARKER_POC, MARKER_RGN, MARKER_SIZ, MARKER_SOC,
};

/// `PPM` marker code (T.800 §A.7.4, `0xFF60`) — packed packet
/// headers in the main header. Not a [`crate`]-level constant because
/// the main-header parser only length-skips it; the decode wiring
/// needs to recognise (and reject) it.
const MARKER_PPM: u16 = 0xFF60;

// ---------------------------------------------------------------------------
// Public output types.
// ---------------------------------------------------------------------------

/// One reconstructed component plane of a decoded image.
///
/// `samples` is row-major `width × height` on the component's own
/// §B.2 grid (Equation B-1 / B-2) — i.e. already divided by the
/// `XRsiz` / `YRsiz` sub-sampling factors. Values are the final
/// §G.1.2 level-shifted samples, clamped to the component's dynamic
/// range: `[0, 2^precision − 1]` for unsigned components,
/// `[−2^(precision−1), 2^(precision−1) − 1]` for signed ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedComponent {
    /// Plane width in samples (component-grid, Equation B-2).
    pub width: u32,
    /// Plane height in samples.
    pub height: u32,
    /// Sample precision in bits (`Ssiz` low 7 bits + 1).
    pub precision_bits: u8,
    /// Whether samples are signed (`Ssiz` MSB).
    pub is_signed: bool,
    /// `XRsiz` — horizontal sub-sampling factor relative to the
    /// reference grid.
    pub h_separation: u8,
    /// `YRsiz` — vertical sub-sampling factor.
    pub v_separation: u8,
    /// Row-major samples, `width * height` entries.
    pub samples: Vec<i32>,
}

/// A fully decoded JPEG 2000 image — one [`DecodedComponent`] per SIZ
/// component, each on its own component grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImage {
    /// Image-area width on the reference grid (`Xsiz − XOsiz`).
    pub width: u32,
    /// Image-area height on the reference grid (`Ysiz − YOsiz`).
    pub height: u32,
    /// Component planes in `Csiz` declaration order.
    pub components: Vec<DecodedComponent>,
}

// ---------------------------------------------------------------------------
// Unsupported-feature detection.
// ---------------------------------------------------------------------------

/// Re-scan the main-header byte span for marker segments the wiring
/// cannot honour yet. [`crate::parse_j2k_header`] length-skips
/// optional markers; silently ignoring `COC` / `RGN` / `POC` / `PPM`
/// / `CAP` would mis-decode the stream, so their presence is surfaced
/// as [`Error::NotImplemented`] here.
///
/// `QCC` is **not** rejected: the main-header per-component
/// quantization override (T.800 §A.6.5, `Main QCC > Main QCD`) is
/// honoured — see [`crate::collect_main_header_qcc`] and
/// [`resolve_component_quant`].
fn reject_unsupported_main_header_markers(bytes: &[u8], header_end: usize) -> Result<(), Error> {
    // SOC is 2 bytes with no length field; every other main-header
    // marker segment is `marker(2) + length(2) + payload(length-2)`.
    let mut pos = 2usize; // skip SOC (already validated by the parser)
    while pos + 4 <= header_end {
        let marker = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]);
        match marker {
            MARKER_COC | MARKER_RGN | MARKER_POC | MARKER_PPM | MARKER_CAP => {
                return Err(Error::NotImplemented);
            }
            MARKER_SOC | MARKER_SIZ => {}
            _ => {}
        }
        let len = u16::from_be_bytes([bytes[pos + 2], bytes[pos + 3]]) as usize;
        if len < 2 {
            return Err(Error::InvalidMarkerLength);
        }
        pos += 2 + len;
    }
    Ok(())
}

/// Reject tile-part-header marker segments that would alter the
/// main-header coding parameters (not wired this round). `PLT` and
/// `COM` are informational and pass through.
fn reject_unsupported_tile_part_markers(markers: &[TilePartMarker]) -> Result<(), Error> {
    for m in markers {
        match m {
            TilePartMarker::Plt(_) | TilePartMarker::Com(_) => {}
            TilePartMarker::Cod(_)
            | TilePartMarker::Coc(_)
            | TilePartMarker::Qcd(_)
            | TilePartMarker::Qcc(_)
            | TilePartMarker::Rgn(_)
            | TilePartMarker::Poc(_)
            | TilePartMarker::Ppt(_) => return Err(Error::NotImplemented),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-sub-band quantisation resolution (§A.6.4 / Annex E).
// ---------------------------------------------------------------------------

/// SPqcd / SPqcc index for the sub-band at resolution level `r` with
/// the given orientation, per the F.3.1 order Table A.28 references:
/// index 0 is the `NLLL` band; resolution level `r ≥ 1` contributes
/// `[HL, LH, HH]` at indices `3(r−1)+1 .. 3(r−1)+3`.
fn spqcd_index(r: u8, orientation: SubBandOrientation) -> usize {
    match orientation {
        SubBandOrientation::LL => 0,
        SubBandOrientation::HL => 3 * (r as usize - 1) + 1,
        SubBandOrientation::LH => 3 * (r as usize - 1) + 2,
        SubBandOrientation::HH => 3 * (r as usize - 1) + 3,
    }
}

/// Resolved per-sub-band quantisation for one component: the
/// Equation E-2 `Mb` (always needed — it anchors the tier-1 starting
/// bit-plane) and, on the 9-7 path, the full [`SubBandQuantization`].
struct BandQuant {
    mb: u32,
    quant: SubBandQuantization,
}

/// The quantisation parameters that apply to one component, after the
/// T.800 §A.6.5 `Main QCC > Main QCD` precedence has been resolved.
/// Borrows the `SPqcd` / `SPqcc` payload from the owning marker.
#[derive(Clone, Copy)]
struct ComponentQuant<'a> {
    style: QuantizationStyle,
    spqcd: &'a [u8],
    guard_bits: u8,
}

/// Resolve the per-component quantisation for every component, applying
/// any main-header `QCC` override over the main `QCD` (T.800 §A.6.5).
///
/// At most one `QCC` may target a given component in the main header
/// (§A.6.5); a duplicate is rejected as malformed. A `QCC` whose
/// `Cqcc` is out of range is likewise rejected.
fn resolve_component_quant<'a>(
    num_components: usize,
    qcd: &'a crate::Qcd,
    qccs: &'a [crate::Qcc],
) -> Result<Vec<ComponentQuant<'a>>, Error> {
    let mut out: Vec<ComponentQuant<'a>> = (0..num_components)
        .map(|_| ComponentQuant {
            style: qcd.style,
            spqcd: &qcd.spqcd,
            guard_bits: qcd.guard_bits,
        })
        .collect();
    let mut seen = vec![false; num_components];
    for qcc in qccs {
        let c = qcc.component_index as usize;
        if c >= num_components {
            return Err(Error::InvalidMarkerLength);
        }
        if seen[c] {
            // §A.6.5: no more than one QCC per component per header.
            return Err(Error::InvalidMarkerLength);
        }
        seen[c] = true;
        out[c] = ComponentQuant {
            style: qcc.style,
            spqcd: &qcc.spqcc,
            guard_bits: qcc.guard_bits,
        };
    }
    Ok(out)
}

/// Resolve `(εb, µb) → (Mb, Rb)` for every sub-band of one component.
///
/// Returns one `Vec<BandQuant>` per resolution level, in the same
/// per-level band order as [`ResolutionLevel::sub_bands`] (`[LL]` at
/// `r = 0`, `[HL, LH, HH]` at `r ≥ 1`).
fn resolve_band_quant(
    levels: &[ResolutionLevel],
    style: QuantizationStyle,
    spqcd: &[u8],
    guard_bits: u8,
    precision: u32,
    n_l: u8,
) -> Result<Vec<Vec<BandQuant>>, Error> {
    // Pre-parse the step-size list once per style.
    let expounded: Vec<StepSize> = match style {
        QuantizationStyle::None => StepSize::parse_reversible_payload(spqcd),
        QuantizationStyle::ScalarExpounded => StepSize::parse_irreversible_payload(spqcd)?,
        QuantizationStyle::ScalarDerived => Vec::new(),
        QuantizationStyle::Reserved(_) => return Err(Error::NotImplemented),
    };
    let derived_base: Option<StepSize> = match style {
        QuantizationStyle::ScalarDerived => Some(StepSize::parse_derived_payload(spqcd)?),
        _ => None,
    };

    let mut out = Vec::with_capacity(levels.len());
    for level in levels {
        let mut per_band = Vec::with_capacity(level.sub_bands.len());
        for band in &level.sub_bands {
            let step = if let Some(base) = derived_base {
                // Scalar derived: Equation E-5 from the NLLL pair.
                dequant::derive_from_nlll(base, n_l, band.nb)?
            } else {
                let idx = spqcd_index(level.r, band.orientation);
                *expounded.get(idx).ok_or(Error::InvalidMarkerLength)?
            };
            let quant =
                SubBandQuantization::resolve(precision, guard_bits, band.orientation, step)?;
            per_band.push(BandQuant {
                mb: quant.mb,
                quant,
            });
        }
        out.push(per_band);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// §B.12 walk → tier-1 accumulation.
// ---------------------------------------------------------------------------

/// Per-code-block accumulator across every layer's packets: total
/// signalled coding passes, the §B.10.5 missing-bit-plane count `P`
/// (first inclusion only), and the per-codeword-segment bytes.
///
/// With the default single-segment style (and the §C.3.6
/// context-reset style, which does **not** split the stream) all of a
/// code-block's packet contributions form one continuous §C.3 codeword
/// segment, so `segments` holds a single entry carrying every pass.
///
/// Under the §D.4.2 "termination on each coding pass" style each
/// included pass owns its own terminated §C.3 segment (§B.10.7.2), so
/// `segments` holds one entry per pass — each a fresh MQ run that the
/// tier-1 driver decodes against its own [`crate::mq::MqDecoder`].
#[derive(Default)]
struct BlockAccum {
    passes: u32,
    p: Option<u32>,
    /// One `(segment bytes, passes carried by this segment)` per §C.3
    /// codeword segment, in coding-pass order.
    segments: Vec<(Vec<u8>, u32)>,
}

/// Key addressing one code-block inside one tile:
/// `(component, resolution, precinct, sub_band, cbx, cby)`.
type BlockKey = (u16, u8, u32, u32, u32, u32);

/// One tier-1-decoded code-block, owned, ready to be bridged into a
/// [`WalkerBlockSource`].
struct DecodedBlock {
    component: u16,
    r: u8,
    precinct: u32,
    sub_band: u32,
    cbx: u32,
    cby: u32,
    nb: u32,
    block: CodeBlock,
}

/// Per-resolution-level [`BlockSource`] dispatch for one
/// tile-component: [`WalkerBlockSource`] keys blocks by orientation
/// only, so one source per resolution level keeps the `HL` blocks of
/// different levels apart. The owning level is recovered from the
/// band's decomposition level `nb` (`r = NL − nb + 1` for high-pass
/// bands, `r = 0` for `LL`, per §B.5).
struct LevelKeyedSource<'a> {
    n_l: u8,
    per_level: Vec<WalkerBlockSource<'a>>,
}

impl<'a> BlockSource<'a> for LevelKeyedSource<'a> {
    fn blocks_for(&self, band: &SubBand) -> &[CodedCodeBlock<'a>] {
        let r = match band.orientation {
            SubBandOrientation::LL => 0usize,
            _ => (self.n_l as usize) + 1 - (band.nb as usize),
        };
        match self.per_level.get(r) {
            Some(src) => src.blocks_for(band),
            None => &[],
        }
    }
}

/// Number of fully-decoded bit-planes implied by `passes` coding
/// passes per the §D.3 schedule (cleanup on the first plane, then
/// `SP / MR / CL` triples): plane `k` completes on its cleanup pass.
fn completed_bitplanes(passes: u32) -> u32 {
    if passes == 0 {
        0
    } else {
        1 + (passes - 1) / 3
    }
}

// ---------------------------------------------------------------------------
// Tile decode.
// ---------------------------------------------------------------------------

/// All COD-level knobs the per-tile decode consumes, resolved once.
struct CodingParams {
    n_l: u8,
    xcb: u8,
    ycb: u8,
    precincts: Vec<u8>,
    layers: u16,
    progression: ProgressionOrder,
    transform: WaveletTransform,
    mct: u8,
    sop_eph: SopEphMode,
    segmentation_symbols: bool,
    vertically_causal: bool,
    reset_context_probabilities: bool,
    /// §D.4.2 "termination on each coding pass" (Table A.19 bit 2):
    /// every coding pass owns its own terminated §C.3 codeword segment
    /// (§B.10.7.2). When set the packet reader uses
    /// [`SegmentSplit::PerPass`] and the tier-1 driver opens a fresh
    /// [`crate::mq::MqDecoder`] per pass.
    termination_on_each_coding_pass: bool,
}

/// Decode every component of one tile from the concatenated tile-part
/// body bytes. Returns one row-major `i32` grid per component
/// (tile-component extent), already §G-level-shifted and clamped.
fn decode_tile(
    siz: &Siz,
    params: &CodingParams,
    comp_quant: &[ComponentQuant<'_>],
    tile_index: u32,
    body: &[u8],
) -> Result<Vec<Vec<i32>>, Error> {
    let tile = derive_tile_geometry(siz, tile_index)?;
    let num_components = siz.components.len();

    // -- Per-component resolution-level geometry + precinct layouts --
    let mut levels_per_comp: Vec<Vec<ResolutionLevel>> = Vec::with_capacity(num_components);
    let mut infos: Vec<ComponentProgressionInfo> = Vec::with_capacity(num_components);
    // Parallel per-component input for the position-keyed (RPCL / PCRL /
    // CPRL) §B.12.1.3–5 orders: same precinct grids, plus the
    // reference-grid corner mapping those orders sort visits by.
    let mut position_infos: Vec<ComponentPositionInfo> = Vec::with_capacity(num_components);
    // (component, resolution, precinct) → precinct code-block geometry.
    let mut precinct_geom: BTreeMap<(u16, u8, u32), PrecinctCodeBlocks> = BTreeMap::new();

    for (c, tc) in tile.components.iter().enumerate() {
        let levels = derive_resolution_levels(*tc, params.n_l);
        let mut per_res = Vec::with_capacity(levels.len());
        let mut res_layouts = Vec::with_capacity(levels.len());
        for level in &levels {
            let pp = precinct_exponents_at(&params.precincts, level.r);
            let partition = derive_precinct_partition(level, pp);
            let num = partition.num_precincts();
            let num = u32::try_from(num).map_err(|_| Error::InvalidMarkerLength)?;
            per_res.push(num);
            // §B.6: the precinct partition is anchored at (0, 0) on the
            // reduced-resolution domain with step 2^PP; the resolution
            // level's left/top edge (trx0/try0) falls in anchor cell
            // floor(trx0 / 2^PPx) — exactly `trx0 >> ppx` for u32.
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
            for k in 0..num {
                let geom = derive_precinct_code_blocks(level, pp, params.xcb, params.ycb, k)?;
                precinct_geom.insert((c as u16, level.r, k), geom);
            }
        }
        infos.push(ComponentProgressionInfo {
            num_decomposition_levels: params.n_l,
            precincts_per_resolution: per_res,
        });
        position_infos.push(ComponentPositionInfo {
            num_decomposition_levels: params.n_l,
            xrsiz: siz.components[c].h_separation,
            yrsiz: siz.components[c].v_separation,
            resolutions: res_layouts,
        });
        levels_per_comp.push(levels);
    }

    // -- §B.12 packet enumeration --
    let descriptors = match params.progression {
        ProgressionOrder::Lrcp => lrcp_packet_order(params.layers, &infos)?,
        ProgressionOrder::Rlcp => rlcp_packet_order(params.layers, &infos)?,
        // §B.12.1.3–5 require XRsiz / YRsiz to be powers of two for every
        // component (the reference-grid corner divisibility tests only
        // hold then). A non-power-of-two sub-sampling factor with one of
        // these orders is malformed; reject rather than mis-order.
        ProgressionOrder::Rpcl | ProgressionOrder::Pcrl | ProgressionOrder::Cprl => {
            for pi in &position_infos {
                if !pi.xrsiz.is_power_of_two() || !pi.yrsiz.is_power_of_two() {
                    return Err(Error::NotImplemented);
                }
            }
            match params.progression {
                ProgressionOrder::Rpcl => rpcl_packet_order(params.layers, &position_infos)?,
                ProgressionOrder::Pcrl => pcrl_packet_order(params.layers, &position_infos)?,
                _ => cprl_packet_order(params.layers, &position_infos)?,
            }
        }
        _ => return Err(Error::NotImplemented),
    };

    // -- §B.10 packet-header walk --
    // Assign one stable precinct-state id per (component, resolution,
    // precinct) triple; build one PacketGeometry per packet.
    let mut state_ids: BTreeMap<(u16, u8, u32), usize> = BTreeMap::new();
    let mut packets: Vec<(usize, PacketGeometry)> = Vec::with_capacity(descriptors.len());
    for desc in &descriptors {
        let key = (desc.component, desc.resolution, desc.precinct);
        let next_id = state_ids.len();
        let id = *state_ids.entry(key).or_insert(next_id);
        let geom = precinct_geom.get(&key).ok_or(Error::InvalidPacketHeader)?;
        let sub_bands = geom
            .sub_bands
            .iter()
            .map(|sb| SubBandGeometry {
                width: sb.grid_wide,
                height: sb.grid_high,
            })
            .collect();
        packets.push((
            id,
            PacketGeometry {
                sub_bands,
                layer: desc.layer,
            },
        ));
    }
    let split = if params.termination_on_each_coding_pass {
        SegmentSplit::PerPass
    } else {
        SegmentSplit::Single
    };
    let headers = crate::packet::walk_packet_headers(body, &packets, params.sop_eph, split)?;

    // -- Replay body offsets; accumulate per-code-block segments --
    let mut accum: BTreeMap<BlockKey, BlockAccum> = BTreeMap::new();
    let mut pos = 0usize;
    for (desc, header) in descriptors.iter().zip(headers.iter()) {
        let mut seg_pos = pos
            .checked_add(header.bytes_consumed)
            .ok_or(Error::PacketHeaderOverrun)?;
        for contrib in &header.contributions {
            if !contrib.included {
                continue;
            }
            let key: BlockKey = (
                desc.component,
                desc.resolution,
                desc.precinct,
                contrib.sub_band,
                contrib.x,
                contrib.y,
            );
            let entry = accum.entry(key).or_default();
            entry.passes = entry
                .passes
                .checked_add(contrib.coding_passes)
                .ok_or(Error::InvalidPacketHeader)?;
            if let Some(p) = contrib.zero_bit_planes {
                if entry.p.is_some() {
                    return Err(Error::InvalidPacketHeader);
                }
                entry.p = Some(p);
            }
            // Slice each §B.10.7 codeword segment out of the packet body
            // and record it with the number of coding passes it carries.
            //
            // * Single segment (§B.10.7.1, default / §C.3.6 context-reset
            //   style) — the code-block's contributions form **one**
            //   continuous §C.3 codeword segment across every layer.
            //   Concatenate this packet's bytes onto a single shared
            //   `segments[0]` entry and add its pass count, so the
            //   multi-layer / multi-packet stream stays one MQ run.
            // * Per-pass segments (§B.10.7.2, §D.4.2 termination) — the
            //   packet reader signalled one length per pass, so each
            //   length carries exactly one terminated pass and the
            //   tier-1 driver opens a fresh MQ decoder per entry. Push
            //   one entry per pass (no cross-layer concatenation: each
            //   terminated pass is independent).
            let num_segs = contrib.segment_lengths.len();
            if num_segs <= 1 {
                let len = contrib.segment_lengths.first().copied().unwrap_or(0) as usize;
                let end = seg_pos.checked_add(len).ok_or(Error::PacketHeaderOverrun)?;
                let bytes = body.get(seg_pos..end).ok_or(Error::PacketHeaderOverrun)?;
                // Append onto the single shared segment for this block.
                if entry.segments.is_empty() {
                    entry.segments.push((Vec::new(), 0));
                }
                let seg = &mut entry.segments[0];
                seg.0.extend_from_slice(bytes);
                seg.1 = seg
                    .1
                    .checked_add(contrib.coding_passes)
                    .ok_or(Error::InvalidPacketHeader)?;
                seg_pos = end;
            } else {
                for (si, &len) in contrib.segment_lengths.iter().enumerate() {
                    let len = len as usize;
                    let end = seg_pos.checked_add(len).ok_or(Error::PacketHeaderOverrun)?;
                    let bytes = body.get(seg_pos..end).ok_or(Error::PacketHeaderOverrun)?;
                    // §B.10.7.2 per-pass split: every length but the last
                    // carries exactly one pass; the residue lands on the
                    // final segment so the sum always equals the
                    // contribution's pass count.
                    let seg_passes = if si + 1 < num_segs {
                        1
                    } else {
                        contrib
                            .coding_passes
                            .checked_sub(si as u32)
                            .ok_or(Error::InvalidPacketHeader)?
                    };
                    entry.segments.push((bytes.to_vec(), seg_passes));
                    seg_pos = end;
                }
            }
        }
        pos = seg_pos;
    }

    // -- Per-component quantisation tables (§A.6.5 QCC override) --
    let mut quant_per_comp: Vec<Vec<Vec<BandQuant>>> = Vec::with_capacity(num_components);
    for (c, levels) in levels_per_comp.iter().enumerate() {
        let precision = siz.components[c].precision_bits as u32;
        let cq = comp_quant.get(c).ok_or(Error::InvalidMarkerLength)?;
        quant_per_comp.push(resolve_band_quant(
            levels,
            cq.style,
            cq.spqcd,
            cq.guard_bits,
            precision,
            params.n_l,
        )?);
    }

    // -- Tier-1: decode every included code-block --
    let mut decoded: Vec<DecodedBlock> = Vec::with_capacity(accum.len());
    for ((c, r, k, sb, cbx, cby), acc) in accum.iter() {
        if acc.passes == 0 {
            continue;
        }
        let geom = precinct_geom
            .get(&(*c, *r, *k))
            .ok_or(Error::InvalidPacketHeader)?;
        let psb = geom
            .sub_bands
            .get(*sb as usize)
            .ok_or(Error::InvalidPacketHeader)?;
        if *cbx >= psb.grid_wide || *cby >= psb.grid_high {
            return Err(Error::InvalidPacketHeader);
        }
        let placement = psb.code_blocks[(*cby as usize) * (psb.grid_wide as usize) + *cbx as usize];
        // Per-level band order matches the §B.9 packet sub-band order
        // ([LL] at r = 0, [HL, LH, HH] at r ≥ 1), so the packet's
        // sub-band index addresses the quant table directly.
        let mb = quant_per_comp[*c as usize][*r as usize]
            .get(*sb as usize)
            .ok_or(Error::InvalidPacketHeader)?
            .mb;
        let p = acc.p.ok_or(Error::InvalidPacketHeader)?;
        if p >= mb {
            return Err(Error::InvalidPacketHeader);
        }
        // §D.3: at most 3 (Mb − P) − 2 passes fit above bit-plane 0.
        if acc.passes > 3 * (mb - p) - 2 {
            return Err(Error::InvalidPacketHeader);
        }
        let mut block = CodeBlock::new(
            psb.orientation,
            placement.width() as usize,
            placement.height() as usize,
        );
        let mut ctx = reset_contexts();
        let mut seq = BitPlaneSequencer::new(mb - 1 - p)
            .with_segmentation_symbols(params.segmentation_symbols)
            .with_vertically_causal_context(params.vertically_causal)
            .with_reset_context_probabilities(params.reset_context_probabilities)
            .with_termination_on_each_coding_pass(params.termination_on_each_coding_pass);
        // Drive the §D.3 pass schedule across this code-block's §C.3
        // codeword segments. Each segment opens a fresh MqDecoder (the
        // MQ engine restarts per §C.3 at every termination boundary —
        // §D.4.1 0xFF-fill is synthesised by `MqDecoder::new`), while
        // the Annex D context array (`ctx`) persists across segments per
        // §D.4 (unless the §C.3.6 reset bit is set, which the sequencer
        // applies internally). The single-segment case is one iteration
        // carrying every pass; the §D.4.2 per-pass case is one iteration
        // per terminated pass.
        for (seg_bytes, seg_passes) in &acc.segments {
            if *seg_passes == 0 {
                continue;
            }
            let mut decoder = crate::mq::MqDecoder::new(seg_bytes);
            seq.decode_passes(&mut block, &mut decoder, &mut ctx, *seg_passes)?;
        }
        // Record the §B.10.5 zero-MSB count so the reassembly bridge can
        // recover the full per-coefficient §D.2.1 Nb(u, v) =
        // P + decoded_bits(u, v). The tier-1 passes have already tracked
        // each coefficient's decoded-bit count; under mid-bit-plane
        // truncation those counts diverge (the §E.1.1.2 / E.1.2.1
        // per-coefficient Nb), tightening the Equation E-6 / E-8 lift
        // versus the per-block `nb` fallback below.
        block.set_zero_bit_planes(p);
        let nb = p + completed_bitplanes(acc.passes);
        decoded.push(DecodedBlock {
            component: *c,
            r: *r,
            precinct: *k,
            sub_band: *sb,
            cbx: *cbx,
            cby: *cby,
            nb,
            block,
        });
    }

    // -- Per-component: bridge → reassemble → §F.3.1 IDWT cascade --
    let mut planes_5x3: Vec<Vec<i32>> = Vec::new();
    let mut planes_9x7: Vec<Vec<f64>> = Vec::new();
    for (c, levels) in levels_per_comp.iter().enumerate() {
        let tc = &tile.components[c];
        let (tw, th) = (tc.width() as usize, tc.height() as usize);
        if tw == 0 || th == 0 {
            match params.transform {
                WaveletTransform::Reversible5x3 => planes_5x3.push(Vec::new()),
                WaveletTransform::Irreversible9x7 => planes_9x7.push(Vec::new()),
                WaveletTransform::Reserved(_) => return Err(Error::NotImplemented),
            }
            continue;
        }

        // Group this component's decoded blocks by (level, precinct).
        // The entry maps are kept alive alongside the per-level
        // sources because the bridge's lifetime parameter unifies the
        // entry-slice borrow with the CodeBlock borrow.
        let entries_store: Vec<BTreeMap<u32, Vec<WalkerBlockEntry<'_>>>> = levels
            .iter()
            .map(|level| {
                let mut by_precinct: BTreeMap<u32, Vec<WalkerBlockEntry<'_>>> = BTreeMap::new();
                for b in decoded
                    .iter()
                    .filter(|b| b.component == c as u16 && b.r == level.r)
                {
                    by_precinct
                        .entry(b.precinct)
                        .or_default()
                        .push(WalkerBlockEntry {
                            sub_band: b.sub_band,
                            cbx: b.cbx,
                            cby: b.cby,
                            coefficients: &b.block,
                            nb: b.nb,
                        });
                }
                by_precinct
            })
            .collect();
        let mut per_level_sources = Vec::with_capacity(levels.len());
        for (level, entries_by_precinct) in levels.iter().zip(entries_store.iter()) {
            let precinct_blocks: Vec<PrecinctBlocks<'_>> = entries_by_precinct
                .iter()
                .map(|(k, entries)| {
                    precinct_geom
                        .get(&(c as u16, level.r, *k))
                        .map(|geometry| PrecinctBlocks {
                            geometry,
                            entries: entries.as_slice(),
                        })
                        .ok_or(Error::InvalidPacketHeader)
                })
                .collect::<Result<_, _>>()?;
            per_level_sources.push(WalkerBlockSource::from_precincts(&precinct_blocks)?);
        }
        let source = LevelKeyedSource {
            n_l: params.n_l,
            per_level: per_level_sources,
        };

        // §A.6.5: a QCC may override the quantisation style per
        // component, but the COD/COC transform is global to this code;
        // Table A.28 still requires each component's style to match the
        // kernel it is reconstructed with.
        let comp_style = comp_quant[c].style;
        match params.transform {
            WaveletTransform::Reversible5x3 => {
                if comp_style != QuantizationStyle::None {
                    // Table A.28: the reversible kernel pairs with the
                    // "no quantisation" style only.
                    return Err(Error::NotImplemented);
                }
                let mb_per_level: Vec<Vec<u32>> = quant_per_comp[c]
                    .iter()
                    .map(|bands| bands.iter().map(|b| b.mb).collect())
                    .collect();
                let grid = idwt_5x3(levels, &source, &mb_per_level, 0.5)?;
                if grid.width != tw || grid.height != th {
                    return Err(Error::InvalidMarkerLength);
                }
                planes_5x3.push(grid.data);
            }
            WaveletTransform::Irreversible9x7 => {
                if comp_style == QuantizationStyle::None {
                    // Table A.28 pairs the 9-7 kernel with scalar
                    // quantisation (derived or expounded).
                    return Err(Error::NotImplemented);
                }
                let quant_per_level: Vec<Vec<SubBandQuantization>> = quant_per_comp[c]
                    .iter()
                    .map(|bands| bands.iter().map(|b| b.quant).collect())
                    .collect();
                let grid = idwt_9x7(levels, &source, &quant_per_level, 0.5)?;
                if grid.width != tw || grid.height != th {
                    return Err(Error::InvalidMarkerLength);
                }
                planes_9x7.push(grid.data);
            }
            WaveletTransform::Reserved(_) => return Err(Error::NotImplemented),
        }
    }

    // -- Annex G: inverse MCT + DC level shift + clamp, per tile --
    let descriptors: Vec<ComponentDescriptor> = siz
        .components
        .iter()
        .map(ComponentDescriptor::from_siz_component)
        .collect();
    match params.transform {
        WaveletTransform::Reversible5x3 => {
            let mode = match params.mct {
                0 => InverseMctMode::None,
                1 => InverseMctMode::Rct,
                _ => return Err(Error::NotImplemented),
            };
            let mut refs: Vec<&mut [i32]> =
                planes_5x3.iter_mut().map(|v| v.as_mut_slice()).collect();
            reconstruct_tile_components_5x3_multi(&mut refs, &descriptors, mode)?;
            Ok(planes_5x3)
        }
        WaveletTransform::Irreversible9x7 => {
            let mode = match params.mct {
                0 => InverseMctMode::None,
                1 => InverseMctMode::Ict,
                _ => return Err(Error::NotImplemented),
            };
            let mut comps_f32: Vec<Vec<f32>> = planes_9x7
                .iter()
                .map(|p| p.iter().map(|&v| v as f32).collect())
                .collect();
            let mut outputs: Vec<Vec<i32>> =
                planes_9x7.iter().map(|p| vec![0i32; p.len()]).collect();
            let mut comp_refs: Vec<&mut [f32]> =
                comps_f32.iter_mut().map(|v| v.as_mut_slice()).collect();
            let mut out_refs: Vec<&mut [i32]> =
                outputs.iter_mut().map(|v| v.as_mut_slice()).collect();
            reconstruct_tile_components_9x7_multi(
                &mut comp_refs,
                &mut out_refs,
                &descriptors,
                mode,
            )?;
            Ok(outputs)
        }
        WaveletTransform::Reserved(_) => Err(Error::NotImplemented),
    }
}

// ---------------------------------------------------------------------------
// Image-level decode.
// ---------------------------------------------------------------------------

/// Decode a raw JPEG 2000 Part-1 codestream (`.j2k` / `.j2c`) into
/// per-component sample planes.
///
/// This is the end-to-end composition of the crate's T.800 stages —
/// see the [module documentation](self) for the geometry classes
/// covered and the features that are cleanly rejected with
/// [`Error::NotImplemented`].
///
/// Every tile of the §B.3 tile grid is decoded independently (its
/// tile-parts concatenated in `TPsot` order) and placed into the
/// per-component image-area planes at the Equation B-12 offsets.
pub fn decode_j2k(bytes: &[u8]) -> Result<DecodedImage, Error> {
    let cs: J2kCodestream = crate::parse_codestream(bytes)?;
    decode_codestream(bytes, &cs)
}

/// [`decode_j2k`] against an already-parsed [`J2kCodestream`] (the
/// `bytes` must be the same buffer the codestream was parsed from).
pub fn decode_codestream(bytes: &[u8], cs: &J2kCodestream) -> Result<DecodedImage, Error> {
    reject_unsupported_main_header_markers(bytes, cs.header.bytes_consumed)?;

    let siz = &cs.header.siz;
    let cod = &cs.header.cod;
    let qcd = &cs.header.qcd;

    // §A.6.5: resolve per-component quantisation, applying any
    // main-header QCC over the main QCD for the components it targets.
    let csiz = siz.components.len() as u16;
    let main_qccs = crate::collect_main_header_qcc(bytes, cs.header.bytes_consumed, csiz)?;
    let comp_quant = resolve_component_quant(siz.components.len(), qcd, &main_qccs)?;

    let style_flags = cod.code_block_style_flags();
    if style_flags.selective_arithmetic_coding_bypass() {
        // §D.6 selective arithmetic-coding bypass splits the code-block
        // into AC + raw (lazy) codeword segments and reads the SP / MR
        // passes from bit-plane 5 onward directly from a bit-stuffed
        // stream. That raw-mode dispatch is not wired into the tier-1
        // driver below yet, so reject it cleanly.
        return Err(Error::NotImplemented);
    }
    // §D.4.2 "termination on each coding pass" (Table A.19 bit 2) splits
    // the contribution into one terminated §C.3 codeword segment per
    // coding pass (§B.10.7.2). The packet reader reads the per-pass
    // §B.10.7 lengths and the tier-1 driver opens a fresh MqDecoder per
    // segment — so it is honoured here.
    //
    // The §C.3.6 reset-context-probabilities bit (0x02) does NOT split
    // the stream — it only re-initialises the Annex D contexts to their
    // Table D.7 states at each pass boundary — and is honoured directly
    // (threaded into the BitPlaneSequencer below).
    let termination_on_each_coding_pass = style_flags.termination_on_each_coding_pass();

    let params = CodingParams {
        n_l: cod.decomposition_levels,
        xcb: cod
            .code_block_width_exp
            .checked_add(2)
            .ok_or(Error::InvalidMarkerLength)?,
        ycb: cod
            .code_block_height_exp
            .checked_add(2)
            .ok_or(Error::InvalidMarkerLength)?,
        precincts: cod.precincts.clone(),
        layers: cod.layers,
        progression: cod.progression,
        transform: cod.transform,
        mct: cod.multi_component_transform,
        sop_eph: match (cod.sop_marker_allowed, cod.eph_marker_used) {
            (false, false) => SopEphMode::None,
            (true, false) => SopEphMode::SopOnly,
            (false, true) => SopEphMode::EphOnly,
            (true, true) => SopEphMode::SopAndEph,
        },
        segmentation_symbols: style_flags.segmentation_symbols(),
        vertically_causal: style_flags.vertically_causal_context(),
        reset_context_probabilities: style_flags.reset_context_probabilities(),
        termination_on_each_coding_pass,
    };

    // -- Image-area planes --
    let areas = image_area(siz)?;
    let (num_x, num_y) = tile_grid_extent(siz)?;
    let num_tiles = (num_x as u64) * (num_y as u64);

    let mut components: Vec<DecodedComponent> = Vec::with_capacity(siz.components.len());
    for (sc, area) in siz.components.iter().zip(areas.iter()) {
        let len = (area.width() as usize)
            .checked_mul(area.height() as usize)
            .ok_or(Error::InvalidMarkerLength)?;
        components.push(DecodedComponent {
            width: area.width(),
            height: area.height(),
            precision_bits: sc.precision_bits,
            is_signed: sc.is_signed,
            h_separation: sc.h_separation,
            v_separation: sc.v_separation,
            samples: vec![0i32; len],
        });
    }

    // -- Group tile-parts by tile, ordered by TPsot --
    let mut parts_by_tile: BTreeMap<u16, Vec<&crate::TilePart>> = BTreeMap::new();
    for tp in &cs.tile_parts {
        reject_unsupported_tile_part_markers(&tp.markers)?;
        parts_by_tile.entry(tp.sot.tile_index).or_default().push(tp);
    }

    for (tile_index, mut parts) in parts_by_tile {
        if (tile_index as u64) >= num_tiles {
            return Err(Error::InvalidTilePartIndex);
        }
        parts.sort_by_key(|tp| tp.sot.tile_part_index);
        let mut body = Vec::new();
        for tp in &parts {
            let end = tp
                .body_offset
                .checked_add(tp.body_len)
                .ok_or(Error::PsotOverflow)?;
            let slice = bytes.get(tp.body_offset..end).ok_or(Error::PsotOverflow)?;
            body.extend_from_slice(slice);
        }

        let tile_planes = decode_tile(siz, &params, &comp_quant, tile_index as u32, &body)?;

        // Place each tile-component plane into its image-area plane at
        // the Equation B-12 offset.
        let tile = derive_tile_geometry(siz, tile_index as u32)?;
        for (c, plane) in tile_planes.iter().enumerate() {
            let tc = &tile.components[c];
            let (tw, th) = (tc.width() as usize, tc.height() as usize);
            if tw == 0 || th == 0 {
                continue;
            }
            if plane.len() != tw * th {
                return Err(Error::InvalidMarkerLength);
            }
            let comp = &mut components[c];
            let area = &areas[c];
            let dx = tc
                .tcx0
                .checked_sub(area.x0)
                .ok_or(Error::InvalidMarkerLength)? as usize;
            let dy = tc
                .tcy0
                .checked_sub(area.y0)
                .ok_or(Error::InvalidMarkerLength)? as usize;
            let cw = comp.width as usize;
            if dx + tw > cw || dy + th > comp.height as usize {
                return Err(Error::InvalidMarkerLength);
            }
            for row in 0..th {
                let src = &plane[row * tw..(row + 1) * tw];
                let dst_start = (dy + row) * cw + dx;
                comp.samples[dst_start..dst_start + tw].copy_from_slice(src);
            }
        }
    }

    Ok(DecodedImage {
        width: siz.x_size.saturating_sub(siz.x_offset),
        height: siz.y_size.saturating_sub(siz.y_offset),
        components,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spqcd_index_follows_f31_order() {
        assert_eq!(spqcd_index(0, SubBandOrientation::LL), 0);
        assert_eq!(spqcd_index(1, SubBandOrientation::HL), 1);
        assert_eq!(spqcd_index(1, SubBandOrientation::LH), 2);
        assert_eq!(spqcd_index(1, SubBandOrientation::HH), 3);
        assert_eq!(spqcd_index(2, SubBandOrientation::HL), 4);
        assert_eq!(spqcd_index(3, SubBandOrientation::HH), 9);
    }

    #[test]
    fn completed_bitplanes_follows_d3_schedule() {
        assert_eq!(completed_bitplanes(0), 0);
        assert_eq!(completed_bitplanes(1), 1); // CL
        assert_eq!(completed_bitplanes(2), 1); // CL SP
        assert_eq!(completed_bitplanes(3), 1); // CL SP MR
        assert_eq!(completed_bitplanes(4), 2); // CL SP MR CL
        assert_eq!(completed_bitplanes(7), 3);
    }

    #[test]
    fn empty_input_is_rejected() {
        assert!(decode_j2k(&[]).is_err());
    }

    // `style_code` is the low-5-bit Table A.28 style; `None` = 0, so the
    // Sqcd/Sqcc byte is just `guard << 5` for the `None` cases below.
    fn qcd(style: QuantizationStyle, guard: u8, spqcd: &[u8]) -> crate::Qcd {
        crate::Qcd {
            sqcd: guard << 5,
            style,
            guard_bits: guard,
            spqcd: spqcd.to_vec(),
        }
    }

    fn qcc(c: u16, style: QuantizationStyle, guard: u8, spqcc: &[u8]) -> crate::Qcc {
        crate::Qcc {
            component_index: c,
            sqcc: guard << 5,
            style,
            guard_bits: guard,
            spqcc: spqcc.to_vec(),
        }
    }

    #[test]
    fn resolve_component_quant_defaults_to_qcd() {
        // §A.6.5: with no QCC, every component inherits the QCD.
        let d = qcd(QuantizationStyle::None, 2, &[0x40, 0x41]);
        let resolved = resolve_component_quant(3, &d, &[]).expect("resolve");
        assert_eq!(resolved.len(), 3);
        for cq in &resolved {
            assert_eq!(cq.style, QuantizationStyle::None);
            assert_eq!(cq.guard_bits, 2);
            assert_eq!(cq.spqcd, &[0x40, 0x41]);
        }
    }

    #[test]
    fn resolve_component_quant_applies_override() {
        // §A.6.5: a QCC for component 1 overrides the QCD only there.
        let d = qcd(QuantizationStyle::None, 2, &[0x40]);
        let c1 = qcc(1, QuantizationStyle::ScalarExpounded, 4, &[0x12, 0x34]);
        let resolved = resolve_component_quant(3, &d, std::slice::from_ref(&c1)).expect("resolve");
        // Components 0 and 2 keep the default.
        assert_eq!(resolved[0].style, QuantizationStyle::None);
        assert_eq!(resolved[0].guard_bits, 2);
        assert_eq!(resolved[2].spqcd, &[0x40]);
        // Component 1 takes the QCC values.
        assert_eq!(resolved[1].style, QuantizationStyle::ScalarExpounded);
        assert_eq!(resolved[1].guard_bits, 4);
        assert_eq!(resolved[1].spqcd, &[0x12, 0x34]);
    }

    #[test]
    fn resolve_component_quant_rejects_out_of_range_index() {
        let d = qcd(QuantizationStyle::None, 1, &[0x40]);
        let bad = qcc(5, QuantizationStyle::None, 1, &[0x40]);
        assert!(resolve_component_quant(3, &d, std::slice::from_ref(&bad)).is_err());
    }

    #[test]
    fn resolve_component_quant_rejects_duplicate_component() {
        // §A.6.5: no more than one QCC per component per header.
        let d = qcd(QuantizationStyle::None, 1, &[0x40]);
        let a = qcc(0, QuantizationStyle::None, 1, &[0x40]);
        let b = qcc(0, QuantizationStyle::ScalarExpounded, 2, &[0x10, 0x20]);
        assert!(resolve_component_quant(2, &d, &[a, b]).is_err());
    }
}
