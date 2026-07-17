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
//! The main-header `QCC` / `COC` per-component overrides (§A.6.5 /
//! §A.6.2 — per-component `NL`, code-block size, precincts and wavelet
//! kernel), the main-header `RGN` Maxshift ROI (§A.6.3 / §H.1), and the
//! per-tile **tile-part header** `COD` / `COC` / `QCD` / `QCC` / `RGN`
//! overrides (§A.6.1 – §A.6.5, resolved along the
//! `Tile-part {COC,QCC} > Tile-part {COD,QCD} > Main {COC,QCC} >
//! Main {COD,QCD}` precedence) are honoured.
//!
//! A `COC` may give different components different wavelet kernels when
//! no multiple-component transform is active (`Rmct = 0`): §G.1.2 then
//! reduces to a per-component DC level-shift + clamp, so each component
//! reconstructs in its own kernel lane and the lanes re-interleave into
//! component order. A mixed-kernel tile that *also* signals an MCT is
//! rejected (Table A.17 pairs the MCT with one kernel across
//! components 0–2).
//!
//! Streams that need machinery this round does not wire are
//! **rejected** with [`Error::NotImplemented`] rather than
//! mis-decoded: a non-Maxshift `RGN` style (`Srgn ≠ 0`), and RPCL /
//! PCRL under non-power-of-two sub-sampling (§B.12.1.3 / §B.12.1.4
//! require power-of-two `XRsiz` / `YRsiz`; CPRL — §B.12.1.5 — carries
//! no such restriction and is decoded at any factor). A `COC` whose
//! Table A.19 code-block **style** byte diverges from the `COD` *is*
//! honoured — each component's segment split and tier-1 dispatch
//! resolve independently (the T.814 §8.2 HTDECLARED HT / Annex D mix
//! included).
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
    cprl_packet_order, lrcp_packet_order, pcrl_packet_order, poc_volume_packet_order,
    rlcp_packet_order, rpcl_packet_order, ComponentPositionInfo, ComponentProgressionInfo,
    PacketDescriptor, PocVolume, ResolutionPrecinctLayout,
};
use crate::reassemble::{
    idwt_5x3, idwt_9x7, BlockSource, CodedCodeBlock, PrecinctBlocks, SubBandQuantization,
    WalkerBlockEntry, WalkerBlockSource,
};
use crate::t1::{reset_contexts, BitPlaneSequencer, CodeBlock};
use crate::{
    Error, J2kCodestream, ProgressionOrder, QuantizationStyle, Siz, WaveletTransform, MARKER_CAP,
    MARKER_SIZ, MARKER_SOC,
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
/// optional markers; silently ignoring `PPM` / `CAP` would mis-decode
/// the stream, so their presence is surfaced as
/// [`Error::NotImplemented`] here.
///
/// `QCC`, `COC`, `RGN` and `POC` are **not** rejected: the main-header
/// per-component quantization override (T.800 §A.6.5, `Main QCC > Main
/// QCD`), the per-component coding-style override (T.800 §A.6.2, `Main
/// COC > Main COD`), the §H.1 region-of-interest Maxshift decode, and
/// the §A.6.6 progression order change are honoured — see
/// [`crate::collect_main_header_qcc`] / [`resolve_component_quant`],
/// [`crate::collect_main_header_coc`] / [`resolve_component_coding`],
/// [`crate::collect_main_header_rgn`] / [`resolve_component_roi_shift`],
/// and [`crate::collect_main_header_poc`] / [`resolve_tile_coding`].
fn reject_unsupported_main_header_markers(bytes: &[u8], header_end: usize) -> Result<(), Error> {
    // SOC is 2 bytes with no length field; every other main-header
    // marker segment is `marker(2) + length(2) + payload(length-2)`.
    let mut pos = 2usize; // skip SOC (already validated by the parser)
    while pos + 4 <= header_end {
        let marker = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]);
        let len = u16::from_be_bytes([bytes[pos + 2], bytes[pos + 3]]) as usize;
        if len < 2 {
            return Err(Error::InvalidMarkerLength);
        }
        match marker {
            // POC (§A.6.6) is honoured — see `collect_main_header_poc` and
            // the `resolve_tile_coding` POC-volume wiring; it is no longer
            // rejected here.
            // PPM (§A.7.4): relocated packet headers for all tiles. The
            // payload is gathered and re-streamed by
            // `collect_main_header_ppm` + the decode driver, so it is no
            // longer rejected here; the length-skip below steps past it.
            MARKER_PPM => {}
            MARKER_CAP => {
                // T.814 §A.3: the only CAP configuration this decoder
                // honours is one that signals HTJ2K (Pcap bit 15 set). A
                // CAP segment that signals some *other* capability we do
                // not implement is rejected; one that signals only HT is
                // accepted (the HT path is driven by the SPcod bit-6 flag
                // per tile-component, parsed from COD/COC).
                let seg_end = pos + 2 + len;
                if seg_end > header_end || len < 6 {
                    return Err(Error::InvalidMarkerLength);
                }
                let pcap = u32::from_be_bytes([
                    bytes[pos + 4],
                    bytes[pos + 5],
                    bytes[pos + 6],
                    bytes[pos + 7],
                ]);
                // Pcap15 = the 15th most-significant bit of the 32-bit
                // Pcap field (bit index 32 − 15 = 17 from the LSB).
                let pcap15 = (pcap >> (32 - 15)) & 1;
                // Any capability bit other than Pcap15 means a Part we do
                // not handle.
                if pcap & !(1u32 << (32 - 15)) != 0 || pcap15 == 0 {
                    return Err(Error::NotImplemented);
                }
            }
            MARKER_SOC | MARKER_SIZ => {}
            _ => {}
        }
        pos += 2 + len;
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

/// Resolve the per-component region-of-interest Maxshift scaling value
/// `s` (T.800 §A.6.3 / §H.1) from the main-header `RGN` markers.
///
/// Returns one `s` per component: `0` for components with no `RGN`
/// (no ROI), or the `SPrgn` shift for those that carry one. Only the
/// `Srgn = 0` implicit-ROI (Maxshift) style is supported — it needs no
/// mask at the decoder (§H.1 is purely amplitude-driven). Any other
/// `Srgn`, an out-of-range `Crgn`, or a duplicate `RGN` for the same
/// component is rejected (`Srgn ≠ 0` as [`Error::NotImplemented`], the
/// structural faults as [`Error::InvalidMarkerLength`]).
fn resolve_component_roi_shift(
    num_components: usize,
    rgns: &[crate::Rgn],
) -> Result<Vec<u32>, Error> {
    let mut out = vec![0u32; num_components];
    let mut seen = vec![false; num_components];
    for rgn in rgns {
        let c = rgn.component_index as usize;
        if c >= num_components {
            return Err(Error::InvalidMarkerLength);
        }
        if seen[c] {
            // §A.6.3: at most one RGN per component in the main header.
            return Err(Error::InvalidMarkerLength);
        }
        seen[c] = true;
        if rgn.srgn != 0 {
            // Table A.25 only defines Srgn = 0 (implicit ROI / Maxshift);
            // any other style is reserved and not wired.
            return Err(Error::NotImplemented);
        }
        out[c] = u32::from(rgn.sprgn);
    }
    Ok(out)
}

/// The per-component coding-style parameters resolved after the T.800
/// §A.6.2 `Main COC > Main COD` precedence: decomposition levels, the
/// code-block size exponents, the user-defined precinct list, the
/// wavelet kernel, and the Table A.19 code-block **style** bits
/// (segmentation symbol, vertically-causal, context reset, per-pass
/// termination, bypass, predictable termination, T.814 HT) — a `COC`
/// carries its own style byte, so each tile-component's segment split
/// and tier-1 dispatch resolve independently.
#[derive(Clone)]
struct ComponentCoding {
    /// `SPcoc` decomposition levels, `NL`.
    n_l: u8,
    /// Code-block width exponent `xcb` = `code_block_width_exp + 2`.
    xcb: u8,
    /// Code-block height exponent `ycb` = `code_block_height_exp + 2`.
    ycb: u8,
    /// User-defined precinct exponents (one byte per resolution level)
    /// or empty for maximum precincts (`PPx = PPy = 15`).
    precincts: Vec<u8>,
    /// Wavelet kernel selected for this component (Table A.20).
    transform: WaveletTransform,
    /// Table A.19 code-block-style flags for this component.
    style: BlockStyle,
}

/// Resolve the per-component coding style for every component, applying
/// any main-header `COC` override over the main `COD` (T.800 §A.6.2,
/// `Main COC > Main COD`).
///
/// At most one `COC` may target a given component in the main header
/// (§A.6.2); a duplicate, or a `Ccoc` out of range, is rejected as
/// malformed.
///
/// A `COC` may change `NL` / code-block size / precincts / kernel
/// **and** the Table A.19 code-block-style byte per component — a
/// tile-component whose `SPcoc` bit 6 diverges from the `COD` mixes HT
/// and Annex D block coding at tile-component granularity, the T.814
/// §8.2 HTDECLARED set. The packet reader's §B.10.7 segment split and
/// the tier-1 dispatch both key off the resolved per-component style.
fn resolve_component_coding(
    num_components: usize,
    cod: &crate::Cod,
    cocs: &[crate::Coc],
) -> Result<Vec<ComponentCoding>, Error> {
    let cod_xcb = cod
        .code_block_width_exp
        .checked_add(2)
        .ok_or(Error::InvalidMarkerLength)?;
    let cod_ycb = cod
        .code_block_height_exp
        .checked_add(2)
        .ok_or(Error::InvalidMarkerLength)?;
    let default = ComponentCoding {
        n_l: cod.decomposition_levels,
        xcb: cod_xcb,
        ycb: cod_ycb,
        precincts: cod.precincts.clone(),
        transform: cod.transform,
        style: BlockStyle::from_style_byte(cod.code_block_style),
    };
    let mut out: Vec<ComponentCoding> = (0..num_components).map(|_| default.clone()).collect();
    let mut seen = vec![false; num_components];
    for coc in cocs {
        let c = coc.component_index as usize;
        if c >= num_components {
            return Err(Error::InvalidMarkerLength);
        }
        if seen[c] {
            // §A.6.2: no more than one COC per component per header.
            return Err(Error::InvalidMarkerLength);
        }
        seen[c] = true;
        let xcb = coc
            .code_block_width_exp
            .checked_add(2)
            .ok_or(Error::InvalidMarkerLength)?;
        let ycb = coc
            .code_block_height_exp
            .checked_add(2)
            .ok_or(Error::InvalidMarkerLength)?;
        out[c] = ComponentCoding {
            n_l: coc.decomposition_levels,
            xcb,
            ycb,
            precincts: coc.precincts.clone(),
            transform: coc.transform,
            style: BlockStyle::from_style_byte(coc.code_block_style),
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
///
/// Under the §D.6 selective-arithmetic-coding-bypass style the segments
/// alternate AC and raw (lazy) spans per Table D.9, so each
/// [`AccumSegment`] carries an `is_raw` flag the tier-1 driver consults
/// to open a [`crate::t1::RawBitReader`] instead of a
/// [`crate::mq::MqDecoder`].
#[derive(Default)]
struct BlockAccum {
    passes: u32,
    p: Option<u32>,
    /// One entry per §C.3 codeword segment, in coding-pass order.
    segments: Vec<AccumSegment>,
    /// Running absolute pass cursor for the §D.6 bypass and T.814 HT
    /// span splits — the count of passes accumulated by prior packets.
    /// Unused outside those splits.
    bypass_cursor: u32,
    /// T.814 §B.1 placeholder-pass resolution (HT split only): `None`
    /// while every accumulated pass is a placeholder pass, `Some(3·P0)`
    /// once the first HT cleanup pass pinned the placeholder count.
    ht_p0: Option<u32>,
}

/// One accumulated §C.3 codeword segment for a code-block.
struct AccumSegment {
    /// The segment's body bytes (concatenated across packets only for
    /// the single-segment, non-split case).
    bytes: Vec<u8>,
    /// Number of coding passes the segment carries.
    passes: u32,
    /// Whether the segment is a §D.6 raw (lazy) span — decoded from a
    /// [`crate::t1::RawBitReader`] rather than an
    /// [`crate::mq::MqDecoder`]. Always `false` outside §D.6 bypass.
    is_raw: bool,
    /// Absolute index of the segment's first coding pass. Only
    /// meaningful for the T.814 HT split (the §B.3 set grouping keys
    /// off pass positions); 0 elsewhere.
    start_pass: u32,
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

/// The COD-level knobs that stay **global** to the whole code
/// (T.800 §A.6.1). The per-component coding style — `NL`, code-block
/// size, precincts, the wavelet kernel **and** the Table A.19
/// code-block-style bits — is resolved separately into
/// [`ComponentCoding`] so a §A.6.2 `COC` override can change them per
/// component.
#[derive(Clone)]
struct CodingParams {
    layers: u16,
    progression: ProgressionOrder,
    mct: u8,
    sop_eph: SopEphMode,
}

/// The Table A.19 code-block-style flags, resolved **per component**:
/// a §A.6.2 `COC` carries its own style byte, so a tile-component may
/// diverge from the `COD` default — including the T.814 §A.4 SPcod /
/// SPcoc bit 6, whose per-tile-component mixing of HT and Annex D
/// block coding is exactly the T.814 §8.2 **HTDECLARED** set. Drives
/// the component's §B.10.7 segment split and its tier-1 dispatch.
#[derive(Clone, Copy)]
struct BlockStyle {
    segmentation_symbols: bool,
    vertically_causal: bool,
    reset_context_probabilities: bool,
    /// §D.4.2 "termination on each coding pass" (Table A.19 bit 2):
    /// every coding pass owns its own terminated §C.3 codeword segment
    /// (§B.10.7.2). When set the packet reader uses
    /// [`SegmentSplit::PerPass`] and the tier-1 driver opens a fresh
    /// [`crate::mq::MqDecoder`] per pass.
    termination_on_each_coding_pass: bool,
    /// §D.6 "selective arithmetic coding bypass" (Table A.19 bit 0):
    /// the SP / MR passes from bit-plane 5 onward read raw (lazy) bits
    /// instead of the MQ arithmetic decoder, and the code-block
    /// contribution carves into the §B.10.7.2 / Table D.9 AC + raw
    /// codeword segments. When set the packet reader uses
    /// [`SegmentSplit::Bypass`] and the tier-1 driver dispatches each
    /// pass to its AC or raw entry point.
    selective_arithmetic_coding_bypass: bool,
    /// §D.4.2 "predictable termination" (Table A.19 bit 4): every
    /// terminated §C.3 codeword segment in the packet body was flushed
    /// by the specific §D.4.2 procedure. The flag constrains the
    /// *encoder's* flush only — decoding is unchanged (the §D.4.1
    /// synthesised 0xFF extension applies as usual, and real
    /// predictable-termination streams routinely finish their final
    /// renormalisations inside it), so the flag is carried for
    /// introspection but drives no decode-side branch. Forced off for
    /// HT code-blocks (Table A.13 "does not apply to HT code-blocks").
    predictable_termination: bool,
    /// T.814 §A.4 SPcod / SPcoc bit 6: the tile-component's code-blocks
    /// are HTJ2K (ITU-T T.814 | ISO/IEC 15444-15) HT code-blocks,
    /// decoded by [`crate::ht`] rather than the Annex D MQ tier-1 path.
    /// When set, the §D.6 / §D.4.2 flags (Table A.19 bits 0/2) do not
    /// apply to the HT code-blocks (Table A.4) and are forced off so
    /// the packet reader uses the HT set-`T` layout.
    high_throughput: bool,
}

impl BlockStyle {
    /// Decode a Table A.19 `SPcod` / `SPcoc` style byte.
    ///
    /// T.814 §A.4: when bit 6 is set the code-blocks are HT code-blocks
    /// and the Table A.4 reading of the byte applies — bits 0/1/2/4/5
    /// (bypass / context-reset / per-pass-termination / predictable-
    /// termination / segmentation-symbols) do NOT apply to HT
    /// code-blocks. Only the vertically-causal bit 3 carries over.
    /// Force the rest off.
    fn from_style_byte(byte: u8) -> Self {
        let style_flags = crate::CodeBlockStyle::from_byte(byte);
        let ht = style_flags.high_throughput();
        BlockStyle {
            segmentation_symbols: !ht && style_flags.segmentation_symbols(),
            vertically_causal: style_flags.vertically_causal_context(),
            reset_context_probabilities: !ht && style_flags.reset_context_probabilities(),
            // §D.4.2 "termination on each coding pass" (Table A.19
            // bit 2) splits the contribution into one terminated §C.3
            // codeword segment per coding pass (§B.10.7.2). The §C.3.6
            // context-reset bit (0x02) does NOT split the stream — it
            // only re-initialises the Annex D contexts to their Table
            // D.7 states at each pass boundary — and is threaded into
            // the `BitPlaneSequencer` rather than the packet reader.
            termination_on_each_coding_pass: !ht && style_flags.termination_on_each_coding_pass(),
            selective_arithmetic_coding_bypass: !ht
                && style_flags.selective_arithmetic_coding_bypass(),
            predictable_termination: !ht && style_flags.predictable_termination(),
            high_throughput: ht,
        }
    }

    /// The §B.10.7 codeword-segment split these style bits select for
    /// the component's code-block contributions.
    fn split(&self) -> SegmentSplit {
        if self.high_throughput {
            // T.814 §B.2 / §B.3 — HT code-blocks split at the set-T
            // boundaries: one codeword segment per HT cleanup pass, one
            // per SigProp (+ MagRef) refinement pair.
            SegmentSplit::Ht
        } else if self.selective_arithmetic_coding_bypass {
            // §D.6 bypass — Table D.9 AC + raw codeword-segment split.
            // Bit-2 ("termination on each coding pass") composes: when
            // also set every pass (including both raw passes)
            // terminates.
            SegmentSplit::Bypass {
                termination_on_each_coding_pass: self.termination_on_each_coding_pass,
            }
        } else if self.termination_on_each_coding_pass {
            SegmentSplit::PerPass
        } else {
            SegmentSplit::Single
        }
    }
}

/// Build the global [`CodingParams`] from a resolved [`Cod`] (the
/// main-header default, or a tile-part `COD` override per §A.6.1).
fn coding_params_from_cod(cod: &crate::Cod) -> Result<CodingParams, Error> {
    Ok(CodingParams {
        layers: cod.layers,
        progression: cod.progression,
        mct: cod.multi_component_transform,
        sop_eph: match (cod.sop_marker_allowed, cod.eph_marker_used) {
            (false, false) => SopEphMode::None,
            (true, false) => SopEphMode::SopOnly,
            (false, true) => SopEphMode::EphOnly,
            (true, true) => SopEphMode::SopAndEph,
        },
    })
}

/// The fully-resolved coding parameters that drive one tile's decode,
/// after the §A.6 `Tile-part {COC,QCC} > Tile-part {COD,QCD} >
/// Main {COC,QCC} > Main {COD,QCD}` precedence has been applied.
///
/// `comp_quant` borrows the `SPqcd` / `SPqcc` payload from whichever
/// marker won the precedence (a main-header `Qcd` / `Qcc`, or a
/// tile-part `Qcd` / `Qcc` held in [`crate::TilePart::markers`]); all
/// of those outlive the per-tile decode loop.
struct ResolvedTileCoding<'a> {
    params: CodingParams,
    comp_coding: Vec<ComponentCoding>,
    comp_quant: Vec<ComponentQuant<'a>>,
    roi_shift: Vec<u32>,
    /// §A.6.6 progression order change volumes that apply to this tile,
    /// after the `Tile-part POC > Main POC` precedence. Empty when no
    /// `POC` governs the tile — in that case `params.progression` (the
    /// COD's `SGcod` order) drives a single LRCP/RLCP/RPCL/PCRL/CPRL
    /// enumeration as before.
    poc_volumes: Vec<PocVolume>,
}

/// Resolve the per-tile coding parameters by layering this tile's
/// first-tile-part (`TPsot = 0`) `COD` / `COC` / `QCD` / `QCC` / `RGN`
/// overrides on top of the resolved main-header defaults, per the §A.6
/// precedence rules.
///
/// * `COD`/`COC` (§A.6.1 / §A.6.2): a tile-part `COD` overrides the
///   main `COD` **and** the main `COC`s for the whole tile; a tile-part
///   `COC` then overrides that per component. So the effective
///   precedence for each component is
///   `Tile COC > Tile COD > Main COC > Main COD` — when a tile `COD` is
///   present the main `COC`s are discarded (the tile `COD` supersedes
///   them), and only the tile `COC`s refine it.
/// * `QCD`/`QCC` (§A.6.4 / §A.6.5): identical shape —
///   `Tile QCC > Tile QCD > Main QCC > Main QCD`.
/// * `RGN` (§A.6.3): a tile-part `RGN` overrides the main `RGN` for its
///   component; components without a tile `RGN` keep the main one.
///
/// Per §A.6.1 / §A.6.2 / §A.6.4 / §A.6.5 these markers, if present in a
/// multi-tile-part tile, appear only in the first tile-part — the
/// Gathers the relocated packet-header payload (T.800 §A.7.5) for one
/// tile, concatenating the `Ippt` bytes of every `PPT` marker segment
/// across the tile's tile-parts.
///
/// Returns `None` when the tile carries no `PPT` (the §B.10 in-stream
/// path applies). When `PPT` is present, the `Zppt` indices across all
/// the tile's segments must form the contiguous run `0..N`, each value
/// appearing exactly once — a gap or duplicate is rejected as a lost or
/// mis-ordered relocated-header segment (§A.7.5 "increasing `Zppt`").
fn gather_ppt_headers(parts: &[&crate::TilePart]) -> Result<Option<Vec<u8>>, Error> {
    // Collect (Zppt, payload) across the tile, preserving codestream
    // order so equal-Zppt duplicates are detected deterministically.
    let mut segs: Vec<(u8, &[u8])> = Vec::new();
    for tp in parts {
        for m in &tp.markers {
            if let crate::TilePartMarker::Ppt(p) = m {
                segs.push((p.z_index, p.packet_headers.as_slice()));
            }
        }
    }
    if segs.is_empty() {
        return Ok(None);
    }
    // §A.7.5: order by Zppt; the set must be exactly 0..N with no gaps
    // or duplicates.
    segs.sort_by_key(|(z, _)| *z);
    for (expected, (z, _)) in segs.iter().enumerate() {
        let expected = u8::try_from(expected).map_err(|_| Error::InvalidMarkerLength)?;
        if *z != expected {
            return Err(Error::InvalidMarkerLength);
        }
    }
    let total: usize = segs.iter().map(|(_, b)| b.len()).sum();
    let mut buf = Vec::with_capacity(total);
    for (_, b) in &segs {
        buf.extend_from_slice(b);
    }
    Ok(Some(buf))
}

/// caller passes that tile-part's `markers`.
#[allow(clippy::too_many_arguments)]
fn resolve_tile_coding<'a>(
    num_components: usize,
    main_rgns: &[crate::Rgn],
    main_poc: Option<&crate::Poc>,
    main_params: &CodingParams,
    main_coding: &[ComponentCoding],
    main_quant: &[ComponentQuant<'a>],
    main_roi: &[u32],
    tile_markers: &'a [crate::TilePartMarker],
) -> Result<ResolvedTileCoding<'a>, Error> {
    // Collect this tile's first-tile-part overrides, enforcing the §A.6
    // "at most one per header" rule for the un-indexed COD / QCD.
    let mut tile_cod: Option<&crate::Cod> = None;
    let mut tile_qcd: Option<&crate::Qcd> = None;
    let mut tile_cocs: Vec<&crate::Coc> = Vec::new();
    let mut tile_qccs: Vec<&crate::Qcc> = Vec::new();
    let mut tile_rgns: Vec<&crate::Rgn> = Vec::new();
    let mut tile_poc: Option<&crate::Poc> = None;
    for m in tile_markers {
        match m {
            crate::TilePartMarker::Cod(c) => {
                if tile_cod.is_some() {
                    return Err(Error::InvalidMarkerLength);
                }
                tile_cod = Some(c);
            }
            crate::TilePartMarker::Qcd(q) => {
                if tile_qcd.is_some() {
                    return Err(Error::InvalidMarkerLength);
                }
                tile_qcd = Some(q);
            }
            crate::TilePartMarker::Coc(c) => tile_cocs.push(c),
            crate::TilePartMarker::Qcc(q) => tile_qccs.push(q),
            crate::TilePartMarker::Rgn(r) => tile_rgns.push(r),
            // §A.6.6: at most one POC per header. A tile-part POC, when
            // present, overrides the main POC for this tile.
            crate::TilePartMarker::Poc(p) => {
                if tile_poc.is_some() {
                    return Err(Error::InvalidMarkerLength);
                }
                tile_poc = Some(p);
            }
            // PLT / COM are informational; the tier-2 walker reads the
            // bit-stream lengths directly, so they need no resolution.
            // PPT carries relocated packet headers (§A.7.5); the caller
            // gathers and concatenates them across the tile's tile-parts
            // and feeds the buffer to `decode_tile`, so coding-parameter
            // resolution ignores it here.
            crate::TilePartMarker::Plt(_)
            | crate::TilePartMarker::Com(_)
            | crate::TilePartMarker::Ppt(_) => {}
        }
    }

    // §A.6.6 progression-order precedence: `Tile-part POC > Main POC`.
    // (The COD-default order is captured separately in `params.progression`
    // and used by the caller when no POC governs the tile.)
    let poc_volumes = poc_volumes_for(tile_poc.or(main_poc));

    // No overrides → the resolved main-header parameters apply verbatim
    // (but a main-header POC may still govern this tile's packet order).
    if tile_cod.is_none()
        && tile_qcd.is_none()
        && tile_cocs.is_empty()
        && tile_qccs.is_empty()
        && tile_rgns.is_empty()
    {
        return Ok(ResolvedTileCoding {
            params: main_params.clone(),
            comp_coding: main_coding.to_vec(),
            comp_quant: main_quant.to_vec(),
            roi_shift: main_roi.to_vec(),
            poc_volumes,
        });
    }

    // -- Coding style (§A.6.1 / §A.6.2) --
    // A tile COD supersedes the main COD *and* the main COCs; only the
    // tile COCs then refine it. With no tile COD, the main COCs survive
    // and the tile COCs override per component on top of them.
    let (params, comp_coding) = if let Some(tcod) = tile_cod {
        let params = coding_params_from_cod(tcod)?;
        let comp_coding = resolve_component_coding(num_components, tcod, &cloned(&tile_cocs))?;
        (params, comp_coding)
    } else {
        // Start from the main-header resolution, then let any tile COC
        // override per component (the tile COC outranks the main COC).
        let mut comp_coding = main_coding.to_vec();
        apply_coc_overrides(&mut comp_coding, &tile_cocs)?;
        (main_params.clone(), comp_coding)
    };

    // -- Quantisation (§A.6.4 / §A.6.5) --
    let comp_quant = if let Some(tqcd) = tile_qcd {
        // A tile QCD supersedes the main QCD *and* the main QCCs; the
        // tile QCCs then refine it per component. Seed every component
        // from the tile QCD, then apply the tile QCCs on top.
        let mut comp_quant: Vec<ComponentQuant<'a>> = (0..num_components)
            .map(|_| ComponentQuant {
                style: tqcd.style,
                spqcd: &tqcd.spqcd,
                guard_bits: tqcd.guard_bits,
            })
            .collect();
        apply_qcc_overrides(&mut comp_quant, num_components, &tile_qccs)?;
        comp_quant
    } else {
        let mut comp_quant = main_quant.to_vec();
        apply_qcc_overrides(&mut comp_quant, num_components, &tile_qccs)?;
        comp_quant
    };

    // -- Region of interest (§A.6.3) --
    let roi_shift = if tile_rgns.is_empty() {
        main_roi.to_vec()
    } else {
        apply_rgn_overrides(num_components, main_rgns, &tile_rgns)?
    };

    Ok(ResolvedTileCoding {
        params,
        comp_coding,
        comp_quant,
        roi_shift,
        poc_volumes,
    })
}

/// Convert the governing `POC` marker (already resolved through the
/// §A.6.6 `Tile-part POC > Main POC` precedence) into the runtime
/// [`PocVolume`] list the §B.12.2 enumerator consumes.
///
/// Returns an empty `Vec` when no `POC` governs the tile — the caller
/// then falls back to the COD-default single-order enumeration.
fn poc_volumes_for(poc: Option<&crate::Poc>) -> Vec<PocVolume> {
    match poc {
        Some(p) => p.progressions.iter().map(PocVolume::from_poc).collect(),
        None => Vec::new(),
    }
}

/// Clone a `Vec<&T>` into the owned `Vec<T>` the `resolve_*` helpers
/// (which take `&[T]`) expect. Used to feed tile-part overrides — held
/// as references into [`crate::TilePart::markers`] — through the same
/// resolvers the main-header path uses.
fn cloned<T: Clone>(refs: &[&T]) -> Vec<T> {
    refs.iter().map(|&r| r.clone()).collect()
}

/// Apply tile-part `COC` overrides on top of an already-resolved
/// per-component coding vector (the no-tile-COD branch). The tile COC
/// outranks both the main COC (already folded into `coding`) and the
/// main COD — including its Table A.19 code-block-style byte, which
/// resolves per component.
fn apply_coc_overrides(
    coding: &mut [ComponentCoding],
    tile_cocs: &[&crate::Coc],
) -> Result<(), Error> {
    let mut seen = vec![false; coding.len()];
    for coc in tile_cocs {
        let c = coc.component_index as usize;
        if c >= coding.len() {
            return Err(Error::InvalidMarkerLength);
        }
        if seen[c] {
            return Err(Error::InvalidMarkerLength);
        }
        seen[c] = true;
        let xcb = coc
            .code_block_width_exp
            .checked_add(2)
            .ok_or(Error::InvalidMarkerLength)?;
        let ycb = coc
            .code_block_height_exp
            .checked_add(2)
            .ok_or(Error::InvalidMarkerLength)?;
        coding[c] = ComponentCoding {
            n_l: coc.decomposition_levels,
            xcb,
            ycb,
            precincts: coc.precincts.clone(),
            transform: coc.transform,
            style: BlockStyle::from_style_byte(coc.code_block_style),
        };
    }
    Ok(())
}

/// Apply tile-part `QCC` overrides on top of an already-resolved
/// per-component quantisation vector (the no-tile-QCD branch). The tile
/// QCC outranks both the main QCC (already folded into `quant`) and the
/// main QCD.
fn apply_qcc_overrides<'a>(
    quant: &mut [ComponentQuant<'a>],
    num_components: usize,
    tile_qccs: &[&'a crate::Qcc],
) -> Result<(), Error> {
    let mut seen = vec![false; num_components];
    for qcc in tile_qccs {
        let c = qcc.component_index as usize;
        if c >= num_components {
            return Err(Error::InvalidMarkerLength);
        }
        if seen[c] {
            return Err(Error::InvalidMarkerLength);
        }
        seen[c] = true;
        quant[c] = ComponentQuant {
            style: qcc.style,
            spqcd: &qcc.spqcc,
            guard_bits: qcc.guard_bits,
        };
    }
    Ok(())
}

/// Apply tile-part `RGN` overrides (§A.6.3). The main-header `RGN`
/// shifts seed the result; a tile `RGN` overrides its component. Only
/// the Maxshift (`Srgn = 0`) style is wired; any other style, an
/// out-of-range `Crgn`, or a duplicate tile `RGN` is rejected.
fn apply_rgn_overrides(
    num_components: usize,
    main_rgns: &[crate::Rgn],
    tile_rgns: &[&crate::Rgn],
) -> Result<Vec<u32>, Error> {
    let mut out = resolve_component_roi_shift(num_components, main_rgns)?;
    let mut seen = vec![false; num_components];
    for rgn in tile_rgns {
        let c = rgn.component_index as usize;
        if c >= num_components {
            return Err(Error::InvalidMarkerLength);
        }
        if seen[c] {
            return Err(Error::InvalidMarkerLength);
        }
        seen[c] = true;
        if rgn.srgn != 0 {
            return Err(Error::NotImplemented);
        }
        out[c] = u32::from(rgn.sprgn);
    }
    Ok(out)
}

/// Decode every component of one tile from the concatenated tile-part
/// body bytes. Returns one row-major `i32` grid per component
/// (tile-component extent), already §G-level-shifted and clamped.
#[allow(clippy::too_many_arguments)]
fn decode_tile(
    siz: &Siz,
    params: &CodingParams,
    comp_coding: &[ComponentCoding],
    comp_quant: &[ComponentQuant<'_>],
    roi_shift: &[u32],
    poc_volumes: &[PocVolume],
    tile_index: u32,
    body: &[u8],
    // Relocated packet-header buffer (concatenated `PPM` / `PPT`
    // `Ippm` / `Ippt` payload, T.800 §A.7.4 / §A.7.5). When `Some`,
    // every packet's header is decoded from here and `body` holds
    // only the packet data; when `None` the headers are interleaved
    // in `body` as in §B.10.
    relocated_headers: Option<&[u8]>,
    // ISO/IEC 15444-4 §B.2.3 reduced-resolution decode: the number of
    // highest resolution levels to discard. 0 = full resolution. The
    // tier-2 walk still parses every packet (the byte stream is
    // sequential); only tier-1 + synthesis stop early.
    discard_levels: u8,
    // Layer-limited decode: only contributions from quality layers
    // `< max_layers` feed tier-1 (a §B.12.1.1 prefix truncation — each
    // code-block sees exactly its first-`max_layers` coding passes,
    // the same shape as a rate-truncated stream). `u16::MAX` = all.
    max_layers: u16,
) -> Result<Vec<Vec<i32>>, Error> {
    // -- §B.6–§B.12 tile packet plan (geometry, enumeration, walk split) --
    let TilePacketPlan {
        levels_per_comp,
        precinct_geom,
        descriptors,
        packets,
    } = build_tile_packet_plan(siz, params, comp_coding, poc_volumes, tile_index)?;

    // -- §A.7.4 / §A.7.5: when packet headers are relocated into PPM / PPT
    // segments the header stream and data stream live in two buffers, so
    // each packet carries an explicit body offset. The in-stream §B.10
    // path recovers the data start from `pos + header.bytes_consumed`
    // instead. Normalise both into a `(header, data_offset)` list so the
    // segment-slicing replay below is identical for the two framings.
    let headers = walk_tile_packet_headers(body, &packets, params.sop_eph, relocated_headers)?;

    decode_tile_from_plan(
        siz,
        params,
        comp_coding,
        comp_quant,
        roi_shift,
        tile_index,
        body,
        &levels_per_comp,
        &precinct_geom,
        &descriptors,
        &headers,
        discard_levels,
        max_layers,
    )
}

/// The geometry + enumeration artefacts a tile's tier-2 walk and tier-1
/// decode consume — produced once by [`build_tile_packet_plan`] and
/// shared by the decode driver and the relocated-header transcoder.
struct TilePacketPlan {
    /// Per-component resolution-level geometry (LL + per-level bands).
    levels_per_comp: Vec<Vec<ResolutionLevel>>,
    /// (component, resolution, precinct) → precinct code-block geometry.
    precinct_geom: BTreeMap<(u16, u8, u32), PrecinctCodeBlocks>,
    /// Packet enumeration order (§B.12 / §B.12.2).
    descriptors: Vec<PacketDescriptor>,
    /// One `(precinct-state-id, geometry, segment-split)` per packet,
    /// in `descriptors` order, for the tier-2 header walk. The
    /// §B.10.7 split is per packet because it follows the packet's
    /// component's resolved Table A.19 style byte (§A.6.2).
    packets: Vec<(usize, PacketGeometry, SegmentSplit)>,
}

/// Builds the [`TilePacketPlan`] for one tile: per-component
/// resolution-level + precinct geometry (§B.6–§B.9), the §B.12 /
/// §B.12.2 packet enumeration order, the per-packet
/// [`PacketGeometry`], and the §B.10.7 segment split.
fn build_tile_packet_plan(
    siz: &Siz,
    params: &CodingParams,
    comp_coding: &[ComponentCoding],
    poc_volumes: &[PocVolume],
    tile_index: u32,
) -> Result<TilePacketPlan, Error> {
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
        let cc = comp_coding.get(c).ok_or(Error::InvalidMarkerLength)?;
        let levels = derive_resolution_levels(*tc, cc.n_l);
        let mut per_res = Vec::with_capacity(levels.len());
        let mut res_layouts = Vec::with_capacity(levels.len());
        for level in &levels {
            let pp = precinct_exponents_at(&cc.precincts, level.r);
            // §B.6: "PPx and PPy must be at least 1 for all resolution
            // levels except r = 0 where they are allowed to be zero"
            // (Table A.21 mirrors this on the marker payload). A zero
            // exponent at r > 0 cannot come from a conforming encoder —
            // decoding on regardless would build a precinct lattice the
            // encoder cannot have used and desynchronise the packet
            // walk, so reject the stream instead.
            if level.r > 0 && (pp.ppx == 0 || pp.ppy == 0) {
                return Err(Error::InvalidPrecinctSize);
            }
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
                let geom = derive_precinct_code_blocks(level, pp, cc.xcb, cc.ycb, k)?;
                precinct_geom.insert((c as u16, level.r, k), geom);
            }
        }
        infos.push(ComponentProgressionInfo {
            num_decomposition_levels: cc.n_l,
            precincts_per_resolution: per_res,
        });
        position_infos.push(ComponentPositionInfo {
            num_decomposition_levels: cc.n_l,
            tile_tx0: tile.tx0,
            tile_ty0: tile.ty0,
            xrsiz: siz.components[c].h_separation,
            yrsiz: siz.components[c].v_separation,
            resolutions: res_layouts,
        });
        levels_per_comp.push(levels);
    }

    // -- §B.12 packet enumeration --
    // §A.6.6 / §B.12.2: when a POC governs this tile, the packet order is
    // the concatenation of its progression-order volumes (each volume's
    // [start, end) component / resolution / layer sub-range emitted in its
    // own Ppoc order, with the §B.12.2 "no packet ever repeated" cursor).
    // Otherwise the COD-default single order (SGcod / Ppoc) drives the
    // whole tile. Two of the three position-keyed orders constrain the
    // sub-sampling: §B.12.1.3 states XRsiz / YRsiz "must be powers of
    // two" for **RPCL** and §B.12.1.4 that they "shall be powers of two"
    // for **PCRL**. §B.12.1.5 (**CPRL**) states no such requirement — a
    // CPRL sweep is component-major, so each component's precincts are
    // emitted in its own (y, x, resolution) order and an arbitrary
    // XRsiz / YRsiz only rescales that one component's reference-grid
    // corners, which the `ref_grid_*` projection handles for any integer
    // factor. So the power-of-two gate fires only for RPCL / PCRL (as the
    // COD default or inside a POC volume); a non-power-of-two factor
    // there is malformed and rejected rather than mis-ordered, while
    // CPRL proceeds with any sub-sampling.
    let needs_pow2_position_order = if poc_volumes.is_empty() {
        matches!(
            params.progression,
            ProgressionOrder::Rpcl | ProgressionOrder::Pcrl
        )
    } else {
        poc_volumes
            .iter()
            .any(|v| matches!(v.order, ProgressionOrder::Rpcl | ProgressionOrder::Pcrl))
    };
    if needs_pow2_position_order {
        for pi in &position_infos {
            if !pi.xrsiz.is_power_of_two() || !pi.yrsiz.is_power_of_two() {
                return Err(Error::NotImplemented);
            }
        }
    }

    let descriptors = if !poc_volumes.is_empty() {
        poc_volume_packet_order(poc_volumes, params.layers, &infos, &position_infos)?
    } else {
        match params.progression {
            ProgressionOrder::Lrcp => lrcp_packet_order(params.layers, &infos)?,
            ProgressionOrder::Rlcp => rlcp_packet_order(params.layers, &infos)?,
            ProgressionOrder::Rpcl => rpcl_packet_order(params.layers, &position_infos)?,
            ProgressionOrder::Pcrl => pcrl_packet_order(params.layers, &position_infos)?,
            ProgressionOrder::Cprl => cprl_packet_order(params.layers, &position_infos)?,
            ProgressionOrder::Reserved(_) => return Err(Error::NotImplemented),
        }
    };

    // -- §B.10 packet-header walk --
    // Assign one stable precinct-state id per (component, resolution,
    // precinct) triple; build one PacketGeometry per packet.
    let mut state_ids: BTreeMap<(u16, u8, u32), usize> = BTreeMap::new();
    let mut packets: Vec<(usize, PacketGeometry, SegmentSplit)> =
        Vec::with_capacity(descriptors.len());
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
        // The §B.10.7 codeword-segment split is a property of the
        // packet's **component** (its resolved Table A.19 style byte,
        // §A.6.2) — packets of different components in one tile may
        // split differently (e.g. the T.814 HTDECLARED HT / Annex D
        // mix).
        let split = comp_coding
            .get(desc.component as usize)
            .ok_or(Error::InvalidMarkerLength)?
            .style
            .split();
        packets.push((
            id,
            PacketGeometry {
                sub_bands,
                layer: desc.layer,
            },
            split,
        ));
    }

    Ok(TilePacketPlan {
        levels_per_comp,
        precinct_geom,
        descriptors,
        packets,
    })
}

/// Walks one tile's packet headers, normalising the two framings into a
/// `(header, data_offset)` list where `data_offset` is the offset into
/// `body` at which each packet's code-block data begins.
///
/// * `relocated_headers = Some(buf)` — the §A.7.4 / §A.7.5 relocated
///   path: headers come from `buf`, body holds only data.
/// * `relocated_headers = None` — the §B.10 in-stream path: headers are
///   interleaved with data in `body`, and the data start is recovered
///   from each header's `bytes_consumed`.
fn walk_tile_packet_headers(
    body: &[u8],
    packets: &[(usize, PacketGeometry, SegmentSplit)],
    sop_eph: SopEphMode,
    relocated_headers: Option<&[u8]>,
) -> Result<Vec<(crate::packet::PacketHeader, usize)>, Error> {
    if let Some(header_bytes) = relocated_headers {
        let walked =
            crate::packet::walk_packet_headers_separate(header_bytes, body, packets, sop_eph)?;
        Ok(walked
            .into_iter()
            .map(|rp| (rp.header, rp.body_offset))
            .collect())
    } else {
        let walked = crate::packet::walk_packet_headers(body, packets, sop_eph)?;
        let mut pos = 0usize;
        let mut out = Vec::with_capacity(walked.len());
        for header in walked {
            let data_offset = pos
                .checked_add(header.bytes_consumed)
                .ok_or(Error::PacketHeaderOverrun)?;
            let body_bytes = usize::try_from(header.total_body_bytes())
                .map_err(|_| Error::PacketHeaderOverrun)?;
            pos = data_offset
                .checked_add(body_bytes)
                .ok_or(Error::PacketHeaderOverrun)?;
            out.push((header, data_offset));
        }
        Ok(out)
    }
}

/// Tier-1 / reassembly half of [`decode_tile`], operating on the
/// already-built [`TilePacketPlan`] and the walked packet headers. Split
/// out so the geometry/walk half can be reused by the relocated-header
/// transcoder.
#[allow(clippy::too_many_arguments)]
fn decode_tile_from_plan(
    siz: &Siz,
    params: &CodingParams,
    comp_coding: &[ComponentCoding],
    comp_quant: &[ComponentQuant<'_>],
    roi_shift: &[u32],
    tile_index: u32,
    body: &[u8],
    levels_per_comp: &[Vec<ResolutionLevel>],
    precinct_geom: &BTreeMap<(u16, u8, u32), PrecinctCodeBlocks>,
    descriptors: &[PacketDescriptor],
    headers: &[(crate::packet::PacketHeader, usize)],
    discard_levels: u8,
    max_layers: u16,
) -> Result<Vec<Vec<i32>>, Error> {
    let num_components = siz.components.len();
    let tile = derive_tile_geometry(siz, tile_index)?;

    // §B.2.3 reduced-resolution decode: every component must carry at
    // least `discard_levels` decompositions (a per-component `COC` may
    // lower `NL`; a component that cannot shed that many levels makes
    // the requested reduction unrepresentable).
    if discard_levels > 0 {
        for cc in comp_coding {
            if discard_levels > cc.n_l {
                return Err(Error::InvalidDecompositionLevels);
            }
        }
    }

    // -- Replay body offsets; accumulate per-code-block segments --
    let mut accum: BTreeMap<BlockKey, BlockAccum> = BTreeMap::new();
    for (desc, (header, data_offset)) in descriptors.iter().zip(headers.iter()) {
        let mut seg_pos = *data_offset;
        // The §B.10.7 codeword-segment split follows this packet's
        // component's resolved Table A.19 style byte (§A.6.2) — the
        // same per-packet choice the header walk made.
        let split = comp_coding
            .get(desc.component as usize)
            .ok_or(Error::InvalidMarkerLength)?
            .style
            .split();
        for contrib in &header.contributions {
            if !contrib.included {
                continue;
            }
            // Layer-limited decode: drop the contribution (the header
            // walk above already consumed its bytes — tier-2 stays in
            // sync) so each code-block accumulates exactly the passes
            // its first `max_layers` layers carried. Contributions
            // arrive in increasing layer order per code-block (the
            // §B.12.2 per-precinct "next unsent layer" cursor is
            // monotone), so this is a per-block prefix truncation —
            // the same decode shape as a rate-truncated stream.
            if desc.layer >= max_layers {
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
            // * §D.6 bypass segments (§B.10.7.2, Table D.9) — the packet
            //   reader signalled `|T|` lengths sized for AC + raw spans.
            //   Recompute the matching `(span_passes, is_raw)` from the
            //   running absolute pass cursor (which carries across
            //   layers) and record each segment with its raw / AC tag so
            //   the tier-1 driver dispatches to the right reader.
            if split == SegmentSplit::Ht {
                // T.814 §B.2 / §B.3: recompute the set-T spans from the
                // running absolute pass cursor (which carries across
                // layers) — one accumulated segment per span, so the HT
                // set grouping below sees each cleanup segment and each
                // refinement segment as separate entries.
                //
                // The placeholder-pass resolution mirrors the packet
                // reader's: while the first HT cleanup pass has not
                // been seen, a contribution whose first signalled
                // length is 0 extends the §B.1 placeholder run (one
                // zero-length segment), and one whose first length is
                // ≥ 2 carries the first cleanup pass at the unique
                // candidate index, pinning `3·P0` (§B.3 — the first HT
                // cleanup segment's length exceeds 1, a placeholder
                // run's is 0).
                let start_pass = entry.bypass_cursor;
                let spans = match entry.ht_p0 {
                    Some(p0) => {
                        crate::packet::ht_segment_spans(start_pass, contrib.coding_passes, p0)
                    }
                    None => {
                        let cup = crate::packet::ht_first_cleanup_candidate(
                            start_pass,
                            contrib.coding_passes,
                        );
                        let first_len = contrib.segment_lengths.first().copied().unwrap_or(0);
                        match cup {
                            Some(cup) if first_len >= 2 => {
                                entry.ht_p0 = Some(cup);
                                let rem = start_pass + contrib.coding_passes - 1 - cup;
                                let mut s = vec![cup - start_pass + 1];
                                if rem > 0 {
                                    s.push(rem);
                                }
                                s
                            }
                            _ => {
                                // Placeholder-run extension: exactly one
                                // zero-length segment.
                                if first_len != 0 {
                                    return Err(Error::InvalidPacketHeader);
                                }
                                vec![contrib.coding_passes]
                            }
                        }
                    }
                };
                if spans.len() != contrib.segment_lengths.len() {
                    return Err(Error::InvalidPacketHeader);
                }
                entry.bypass_cursor = entry
                    .bypass_cursor
                    .checked_add(contrib.coding_passes)
                    .ok_or(Error::InvalidPacketHeader)?;
                let mut span_start = start_pass;
                for (&len, span_passes) in contrib.segment_lengths.iter().zip(spans) {
                    let len = len as usize;
                    let end = seg_pos.checked_add(len).ok_or(Error::PacketHeaderOverrun)?;
                    let bytes = body.get(seg_pos..end).ok_or(Error::PacketHeaderOverrun)?;
                    entry.segments.push(AccumSegment {
                        bytes: bytes.to_vec(),
                        passes: span_passes,
                        is_raw: false,
                        start_pass: span_start,
                    });
                    span_start += span_passes;
                    seg_pos = end;
                }
                continue;
            }
            if let SegmentSplit::Bypass {
                termination_on_each_coding_pass,
            } = split
            {
                let spans = crate::packet::bypass_segment_spans(
                    entry.bypass_cursor,
                    contrib.coding_passes,
                    termination_on_each_coding_pass,
                );
                if spans.len() != contrib.segment_lengths.len() {
                    return Err(Error::InvalidPacketHeader);
                }
                entry.bypass_cursor = entry
                    .bypass_cursor
                    .checked_add(contrib.coding_passes)
                    .ok_or(Error::InvalidPacketHeader)?;
                for (&len, (span_passes, is_raw)) in contrib.segment_lengths.iter().zip(spans) {
                    let len = len as usize;
                    let end = seg_pos.checked_add(len).ok_or(Error::PacketHeaderOverrun)?;
                    let bytes = body.get(seg_pos..end).ok_or(Error::PacketHeaderOverrun)?;
                    entry.segments.push(AccumSegment {
                        bytes: bytes.to_vec(),
                        passes: span_passes,
                        is_raw,
                        start_pass: 0,
                    });
                    seg_pos = end;
                }
                continue;
            }
            let num_segs = contrib.segment_lengths.len();
            if num_segs <= 1 {
                let len = contrib.segment_lengths.first().copied().unwrap_or(0) as usize;
                let end = seg_pos.checked_add(len).ok_or(Error::PacketHeaderOverrun)?;
                let bytes = body.get(seg_pos..end).ok_or(Error::PacketHeaderOverrun)?;
                // Append onto the single shared segment for this block.
                if entry.segments.is_empty() {
                    entry.segments.push(AccumSegment {
                        bytes: Vec::new(),
                        passes: 0,
                        is_raw: false,
                        start_pass: 0,
                    });
                }
                let seg = &mut entry.segments[0];
                seg.bytes.extend_from_slice(bytes);
                seg.passes = seg
                    .passes
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
                    entry.segments.push(AccumSegment {
                        bytes: bytes.to_vec(),
                        passes: seg_passes,
                        is_raw: false,
                        start_pass: 0,
                    });
                    seg_pos = end;
                }
            }
        }
        // `seg_pos` now sits at this packet's data end; the next packet's
        // data offset is carried explicitly in `headers`, so no running
        // cursor needs to be threaded across iterations.
        let _ = seg_pos;
    }

    // -- Per-component quantisation tables (§A.6.5 QCC override) --
    let mut quant_per_comp: Vec<Vec<Vec<BandQuant>>> = Vec::with_capacity(num_components);
    for (c, levels) in levels_per_comp.iter().enumerate() {
        let precision = siz.components[c].precision_bits as u32;
        let cq = comp_quant.get(c).ok_or(Error::InvalidMarkerLength)?;
        let cc = comp_coding.get(c).ok_or(Error::InvalidMarkerLength)?;
        quant_per_comp.push(resolve_band_quant(
            levels,
            cq.style,
            cq.spqcd,
            cq.guard_bits,
            precision,
            cc.n_l,
        )?);
    }

    // -- Tier-1: decode every included code-block --
    let mut decoded: Vec<DecodedBlock> = Vec::with_capacity(accum.len());
    for ((c, r, k, sb, cbx, cby), acc) in accum.iter() {
        if acc.passes == 0 {
            continue;
        }
        // Reduced-resolution decode: blocks above the kept resolution
        // level contribute nothing to the truncated synthesis — skip
        // their (already length-accounted) tier-1 work entirely.
        {
            let keep = comp_coding
                .get(*c as usize)
                .ok_or(Error::InvalidMarkerLength)?
                .n_l
                - discard_levels;
            if *r > keep {
                continue;
            }
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
        // §H.1 / §H.2: under an `RGN` Maxshift the encoder shifts ROI
        // coefficients up into the top `s` bit-planes, so the coded
        // bit budget is `M'b = Mb + s`. The tier-1 schedule, the
        // zero-bit-plane bound and the pass-count cap all run against
        // `M'b`; the §H.1 de-scaling below re-anchors the magnitudes to
        // the background `Mb` before reassembly. With no `RGN` `s = 0`
        // and `mb_coded == mb`.
        let s = roi_shift.get(*c as usize).copied().unwrap_or(0);
        let mb_coded = mb.saturating_add(s);
        let p = acc.p.ok_or(Error::InvalidPacketHeader)?;
        if p >= mb_coded {
            return Err(Error::InvalidPacketHeader);
        }
        // The Table A.19 style byte resolved for this block's
        // component (§A.6.2) — tier-1 dispatch is per component.
        let style = comp_coding
            .get(*c as usize)
            .ok_or(Error::InvalidMarkerLength)?
            .style;
        // §D.3: at most 3 (M'b − P) − 2 passes fit above bit-plane 0.
        // The HT path has its own §B.3 cap (Z_blk ≤ 3) and a different
        // pass↔bit-plane relationship, so the Annex D cap does not apply.
        if !style.high_throughput && acc.passes > 3 * (mb_coded - p) - 2 {
            return Err(Error::InvalidPacketHeader);
        }
        // -- HTJ2K (T.814) HT code-block path --
        // When the tile-component signals HT block coding (SPcod bit 6),
        // every code-block is an HT code-block (§A.4 HTONLY/HTDECLARED).
        // The accumulated codeword segments group into §B.1 HT sets:
        // after the 3·P0 placeholder passes, set `j` covers the three
        // coding passes at absolute indices `3P0+3j .. 3P0+3j+2`
        // (cleanup, SigProp, MagRef; the last set may be shorter). Each
        // set's HT cleanup segment is the codeword segment ending at
        // its cleanup pass and its HT refinement segment concatenates
        // the codeword segments covering its refinement passes (§B.3 —
        // a layer boundary may cut between SigProp and MagRef).
        if style.high_throughput {
            // A block whose passes never resolved a first HT cleanup
            // pass carries only placeholder passes — no HT segments,
            // all samples 0 (§7.1.1 with Z_blk = 0, §B.3 NOTE 4).
            let Some(p0) = acc.ht_p0 else {
                continue;
            };
            // Group the codeword segments into per-set cleanup /
            // refinement HT segments. `p0` is a multiple of 3 by
            // construction, so `last − p0` keys the set index and the
            // in-set role: ≡ 0 (mod 3) ends a cleanup segment, else it
            // ends (part of) the refinement segment of the same set.
            let avail = acc.passes - p0;
            // Every HT set (and every placeholder triple) skips one
            // more magnitude bit-plane (§B.3), so a conformant block
            // cannot carry more of either than the band has bit-planes
            // — and the per-set allocations below must not scale with
            // an attacker-controlled pass count.
            if avail > 3 * mb_coded || p0 > 3 * mb_coded {
                return Err(Error::InvalidPacketHeader);
            }
            let num_sets = avail.div_ceil(3) as usize;
            let mut cleanup_seg: Vec<Vec<u8>> = vec![Vec::new(); num_sets];
            let mut refine_seg: Vec<Vec<u8>> = vec![Vec::new(); num_sets];
            for seg in &acc.segments {
                if seg.passes == 0 {
                    continue;
                }
                let seg_last = seg.start_pass + seg.passes - 1;
                if seg_last < p0 {
                    // Pure placeholder run — carries no bytes.
                    if !seg.bytes.is_empty() {
                        return Err(Error::InvalidPacketHeader);
                    }
                    continue;
                }
                let rel = seg_last - p0;
                let set = (rel / 3) as usize;
                if rel % 3 == 0 {
                    cleanup_seg[set].extend_from_slice(&seg.bytes);
                } else {
                    refine_seg[set].extend_from_slice(&seg.bytes);
                }
            }
            // §B.3 validity: the first HT cleanup segment must be
            // longer than 1 byte; a later one is 0 (a bit-plane-skip
            // set) or longer than 1; a refinement segment may only be
            // non-empty when its set's cleanup segment is.
            let mut chosen: Option<usize> = None;
            for j in 0..num_sets {
                let cl = cleanup_seg[j].len();
                if cl == 1 || (j == 0 && cl == 0) {
                    return Err(Error::InvalidPacketHeader);
                }
                if cl == 0 && !refine_seg[j].is_empty() {
                    return Err(Error::InvalidPacketHeader);
                }
                if cl > 0 {
                    chosen = Some(j);
                }
            }
            // Every HT set re-codes the block one magnitude bit-plane
            // finer than its predecessor (S_blk grows by one per §B.3),
            // so the maximal-fidelity choice — and the one a
            // single-set stream reduces to — is the last set whose
            // cleanup segment is present.
            let Some(j) = chosen else {
                continue;
            };
            let passes_in_set = (avail - 3 * j as u32).min(3);
            // §B.3: Z_blk = 1 when the cleanup segment is the only
            // non-empty segment of the set (SigProp / MagRef passes
            // tied to a zero-length refinement segment are not
            // processed), the pass count of the set otherwise.
            let z_blk = if refine_seg[j].is_empty() {
                1
            } else {
                passes_in_set as u8
            };
            // §B.3: S_blk = P + P0 + S_skip.
            let s_blk = p + p0 / 3 + j as u32;
            if s_blk >= mb_coded {
                // More skipped bit-planes than the sub-band has
                // (Equation E-2) — the §7.6 output would not fit Mb.
                return Err(Error::InvalidPacketHeader);
            }
            let (mut block, nb_ht) = crate::ht::decode_ht_codeblock(
                psb.orientation,
                placement.width() as usize,
                placement.height() as usize,
                mb_coded,
                &cleanup_seg[j],
                &refine_seg[j],
                z_blk,
                s_blk,
            )?;
            // §7.6 Nb(u, v) = S_blk + 1 + z_n: the HT block decoder
            // stores the per-sample `1 + z_n` part, the recorded base
            // supplies the S_blk (the §B.10.5 P plus the placeholder /
            // set-skip planes).
            block.set_zero_bit_planes(s_blk);
            // §H.1 Maxshift de-scaling (no-op when s == 0).
            block.apply_roi_maxshift(mb, s);
            decoded.push(DecodedBlock {
                component: *c,
                r: *r,
                precinct: *k,
                sub_band: *sb,
                cbx: *cbx,
                cby: *cby,
                nb: nb_ht,
                block,
            });
            continue;
        }

        let mut block = CodeBlock::new(
            psb.orientation,
            placement.width() as usize,
            placement.height() as usize,
        );
        let mut ctx = reset_contexts();
        let mut seq = BitPlaneSequencer::new(mb_coded - 1 - p)
            .with_segmentation_symbols(style.segmentation_symbols)
            .with_vertically_causal_context(style.vertically_causal)
            .with_reset_context_probabilities(style.reset_context_probabilities)
            .with_termination_on_each_coding_pass(style.termination_on_each_coding_pass)
            .with_selective_arithmetic_coding_bypass(style.selective_arithmetic_coding_bypass)
            .with_predictable_termination(style.predictable_termination);
        // Drive the §D.3 pass schedule across this code-block's §C.3
        // codeword segments. Each AC segment opens a fresh MqDecoder (the
        // MQ engine restarts per §C.3 at every termination boundary —
        // §D.4.1 0xFF-fill is synthesised by `MqDecoder::new`), while
        // the Annex D context array (`ctx`) persists across segments per
        // §D.4 (unless the §C.3.6 reset bit is set, which the sequencer
        // applies internally). The single-segment case is one iteration
        // carrying every pass; the §D.4.2 per-pass case is one iteration
        // per terminated pass; the §D.6 bypass case alternates AC and
        // raw spans (each raw span opens a fresh RawBitReader instead).
        for seg in &acc.segments {
            if seg.passes == 0 {
                continue;
            }
            if seg.is_raw {
                // §D.6 raw (lazy) span — the SP / MR passes read from a
                // bit-stuffed stream. The MQ context array is untouched.
                let mut raw = crate::t1::RawBitReader::new_with_d4_1_fill(&seg.bytes);
                seq.decode_passes_raw(&mut block, &mut raw, seg.passes)?;
            } else {
                let mut decoder = crate::mq::MqDecoder::new(&seg.bytes);
                seq.decode_passes(&mut block, &mut decoder, &mut ctx, seg.passes)?;
                // §D.4.2 predictable termination (Table A.19 bit 4)
                // constrains the *encoder's* flush procedure only — the
                // decode path is unchanged and the §D.4.1 synthesised
                // 0xFF extension still applies ("Often at that point
                // there are more symbols to be decoded. Therefore, the
                // decoder shall extend the input bit stream … with 0xFF
                // bytes"). Real predictable-termination codestreams
                // routinely finish their final renormalisations inside
                // that synthesised fill, so no landing-position check can
                // be made without rejecting conforming streams; §J.7
                // names the segmentation symbol (§D.5) as the in-stream
                // error-detection mechanism instead.
            }
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
        // §H.1 region-of-interest (Maxshift) decode: re-anchor every
        // coefficient from the `M'b = Mb + s` coded budget back to the
        // background `Mb` and rewrite the per-coefficient Nb(u, v). A
        // no-op when `s == 0` (no `RGN` for this component).
        block.apply_roi_maxshift(mb, s);
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
    //
    // Each component's reconstructed samples land in one of two lanes,
    // keyed by its §A.6.2 wavelet kernel: the 5-3 lane (`i32`) or the
    // 9-7 lane (`f64`, integerised later). `comp_lane[c]` records which
    // lane the component's plane went into and its index within that
    // lane, so a tile whose COC gave different components different
    // kernels can be re-interleaved back into component order after the
    // §G reconstruct (see below). When every component shares one kernel
    // one lane is empty and `comp_lane` is the identity within the other.
    let mut planes_5x3: Vec<Vec<i32>> = Vec::new();
    let mut planes_9x7: Vec<Vec<f64>> = Vec::new();
    let mut comp_lane: Vec<(WaveletTransform, usize)> = Vec::with_capacity(levels_per_comp.len());
    for (c, levels) in levels_per_comp.iter().enumerate() {
        let cc = comp_coding.get(c).ok_or(Error::InvalidMarkerLength)?;
        // Reduced-resolution decode: truncate the synthesis cascade to
        // the kept levels — the output is the resolution-level-`keep`
        // reconstruction (§F.3.1 stopped early; ISO/IEC 15444-4
        // §B.2.3). At `discard_levels == 0` this is the full slice and
        // the level-`NL` (tile-component) extent.
        let keep = (cc.n_l - discard_levels) as usize;
        let levels = &levels[..=keep];
        let out_level = &levels[keep];
        let tc = &tile.components[c];
        let (tw, th) = if discard_levels == 0 {
            (tc.width() as usize, tc.height() as usize)
        } else {
            (out_level.width() as usize, out_level.height() as usize)
        };
        if tw == 0 || th == 0 {
            match cc.transform {
                WaveletTransform::Reversible5x3 => {
                    comp_lane.push((WaveletTransform::Reversible5x3, planes_5x3.len()));
                    planes_5x3.push(Vec::new());
                }
                WaveletTransform::Irreversible9x7 => {
                    comp_lane.push((WaveletTransform::Irreversible9x7, planes_9x7.len()));
                    planes_9x7.push(Vec::new());
                }
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
            n_l: cc.n_l,
            per_level: per_level_sources,
        };

        // §A.6.2 / §A.6.5: a COC may override the wavelet kernel and a
        // QCC the quantisation style per component; Table A.28 still
        // requires each component's quantisation style to match the
        // kernel it is reconstructed with.
        let comp_style = comp_quant[c].style;
        match cc.transform {
            WaveletTransform::Reversible5x3 => {
                if comp_style != QuantizationStyle::None {
                    // Table A.28: the reversible kernel pairs with the
                    // "no quantisation" style only.
                    return Err(Error::NotImplemented);
                }
                let mb_per_level: Vec<Vec<u32>> = quant_per_comp[c]
                    .iter()
                    .take(levels.len())
                    .map(|bands| bands.iter().map(|b| b.mb).collect())
                    .collect();
                let grid = idwt_5x3(levels, &source, &mb_per_level, 0.5)?;
                if grid.width != tw || grid.height != th {
                    return Err(Error::InvalidMarkerLength);
                }
                comp_lane.push((WaveletTransform::Reversible5x3, planes_5x3.len()));
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
                    .take(levels.len())
                    .map(|bands| bands.iter().map(|b| b.quant).collect())
                    .collect();
                let grid = idwt_9x7(levels, &source, &quant_per_level, 0.5)?;
                if grid.width != tw || grid.height != th {
                    return Err(Error::InvalidMarkerLength);
                }
                comp_lane.push((WaveletTransform::Irreversible9x7, planes_9x7.len()));
                planes_9x7.push(grid.data);
            }
            WaveletTransform::Reserved(_) => return Err(Error::NotImplemented),
        }
    }

    // -- Annex G: inverse MCT + DC level shift + clamp, per tile --
    //
    // The reassembly above split each component into the `planes_5x3`
    // or `planes_9x7` lane by its own §A.6.2 kernel, tracking the lane +
    // in-lane index in `comp_lane`. Two cases:
    //
    //   * **Uniform kernel** — every component shares one kernel, so one
    //     lane holds all components in component order and the other is
    //     empty. The §G reconstruct (RCT / ICT / none) runs on that lane
    //     and the result is already component-ordered.
    //   * **Mixed kernel** — a COC gave different components different
    //     wavelet kernels, so both lanes are populated. The Annex G MCT
    //     (RCT / ICT) mixes the first three *component* planes together,
    //     which only stays well-defined when those three share one kernel
    //     and one lane; a mixed-kernel tile that also signals an MCT is
    //     rejected. With **no** MCT (`Rmct = 0`), though, §G.1.2 reduces
    //     to a per-component inverse DC level shift + clamp with no
    //     cross-component coupling, so each lane is reconstructed
    //     independently (`InverseMctMode::None`) and the two lanes are
    //     re-interleaved into component order via `comp_lane`.
    let mixed_kernel = {
        let first = comp_coding.first().map(|c| c.transform);
        first.is_some_and(|t| comp_coding.iter().any(|c| c.transform != t))
    };
    let descriptors: Vec<ComponentDescriptor> = siz
        .components
        .iter()
        .map(ComponentDescriptor::from_siz_component)
        .collect();

    // Descriptors for each lane, in that lane's push order (component
    // order within the lane). The `_multi` reconstructs expect their
    // descriptor slice to line up 1:1 with the plane slice.
    let desc_5x3: Vec<ComponentDescriptor> = comp_lane
        .iter()
        .enumerate()
        .filter(|(_, (k, _))| matches!(k, WaveletTransform::Reversible5x3))
        .map(|(c, _)| descriptors[c])
        .collect();
    let desc_9x7: Vec<ComponentDescriptor> = comp_lane
        .iter()
        .enumerate()
        .filter(|(_, (k, _))| matches!(k, WaveletTransform::Irreversible9x7))
        .map(|(c, _)| descriptors[c])
        .collect();

    if mixed_kernel {
        // §G: a mixed-kernel tile with an active MCT (RCT / ICT) would
        // feed planes of two different lanes into the transform's first
        // three inputs — undefined. Reject it; the common same-kernel
        // COC override reconstructs fully. With no MCT the two lanes are
        // independent and both are reconstructed below.
        if params.mct != 0 {
            return Err(Error::NotImplemented);
        }
    }

    // Reconstruct the 5-3 lane (`i32`) in place when populated.
    if !planes_5x3.is_empty() {
        let mode = if mixed_kernel {
            InverseMctMode::None
        } else {
            match params.mct {
                0 => InverseMctMode::None,
                1 => InverseMctMode::Rct,
                _ => return Err(Error::NotImplemented),
            }
        };
        let mut refs: Vec<&mut [i32]> = planes_5x3.iter_mut().map(|v| v.as_mut_slice()).collect();
        reconstruct_tile_components_5x3_multi(&mut refs, &desc_5x3, mode)?;
    }

    // Reconstruct the 9-7 lane (`f64` → `f32` → `i32`) when populated.
    let outputs_9x7: Vec<Vec<i32>> = if planes_9x7.is_empty() {
        Vec::new()
    } else {
        let mode = if mixed_kernel {
            InverseMctMode::None
        } else {
            match params.mct {
                0 => InverseMctMode::None,
                1 => InverseMctMode::Ict,
                _ => return Err(Error::NotImplemented),
            }
        };
        let mut comps_f32: Vec<Vec<f32>> = planes_9x7
            .iter()
            .map(|p| p.iter().map(|&v| v as f32).collect())
            .collect();
        let mut outputs: Vec<Vec<i32>> = planes_9x7.iter().map(|p| vec![0i32; p.len()]).collect();
        let mut comp_refs: Vec<&mut [f32]> =
            comps_f32.iter_mut().map(|v| v.as_mut_slice()).collect();
        let mut out_refs: Vec<&mut [i32]> = outputs.iter_mut().map(|v| v.as_mut_slice()).collect();
        reconstruct_tile_components_9x7_multi(&mut comp_refs, &mut out_refs, &desc_9x7, mode)?;
        outputs
    };

    // Re-interleave the two lanes into component order via `comp_lane`.
    // In the uniform-kernel case one lane is empty and this is a plain
    // move of the populated lane; in the mixed-kernel case it stitches
    // the 5-3 and 9-7 outputs back into the SIZ component sequence the
    // caller places into the image-area planes.
    let mut result: Vec<Vec<i32>> = Vec::with_capacity(comp_lane.len());
    for &(kind, idx) in &comp_lane {
        match kind {
            WaveletTransform::Reversible5x3 => {
                result.push(std::mem::take(&mut planes_5x3[idx]));
            }
            WaveletTransform::Irreversible9x7 => {
                let out = outputs_9x7.get(idx).ok_or(Error::InvalidMarkerLength)?;
                result.push(out.clone());
            }
            WaveletTransform::Reserved(_) => return Err(Error::NotImplemented),
        }
    }
    Ok(result)
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

/// Decode a J2K codestream at **reduced resolution**, discarding the
/// `discard_levels` highest resolution levels of every tile-component
/// (the ISO/IEC 15444-4 §B.2.3 "reduced resolution" decode surface —
/// its Class-0 reference images are decoded exactly this way, and the
/// suffix `rN` on a 15444-4 reference file is this parameter).
///
/// The §F.3.1 synthesis cascade stops `discard_levels` short, so the
/// output component grids are the resolution-level
/// `NL − discard_levels` extents — each dimension is
/// `ceil(full / 2^discard_levels)` on the reference grid (Equation
/// B-14), including the image / tile origin offsets, which scale by
/// the same ceiling division. Tier-2 still parses every packet (the
/// byte stream is sequential); the discarded levels' code-blocks skip
/// tier-1 entirely.
///
/// `discard_levels == 0` is exactly [`decode_j2k`]. A component whose
/// (per-`COC`) decomposition count is smaller than `discard_levels`
/// makes the reduction unrepresentable and surfaces
/// [`Error::InvalidDecompositionLevels`].
pub fn decode_j2k_reduced(bytes: &[u8], discard_levels: u8) -> Result<DecodedImage, Error> {
    let cs: J2kCodestream = crate::parse_codestream(bytes)?;
    decode_codestream_impl(bytes, &cs, discard_levels, u16::MAX)
}

/// Decode a J2K codestream from its first `max_layers` quality layers
/// only — the layer-progressive counterpart of [`decode_j2k_reduced`]
/// (ISO/IEC 15444-4's Class-0 procedures likewise permit decoding a
/// codestream prefix; §B.2.2's relevant-packet rule).
///
/// The tier-2 walk still parses every packet header (the byte stream
/// is sequential), but only contributions from layers `< max_layers`
/// feed tier-1, so each code-block decodes exactly the coding passes
/// its first `max_layers` layers carried — the same §E.1.1.2 /
/// §E.1.2.1 truncated-reconstruction shape as a rate-limited stream
/// (per-coefficient `Nb(u, v)` midpoint lift included). A
/// `max_layers` at or above the codestream's layer count decodes
/// identically to [`decode_j2k`]; `max_layers == 0` is rejected with
/// [`Error::InvalidMarkerLength`] (an image cannot be reconstructed
/// from zero layers).
pub fn decode_j2k_layers(bytes: &[u8], max_layers: u16) -> Result<DecodedImage, Error> {
    if max_layers == 0 {
        return Err(Error::InvalidMarkerLength);
    }
    let cs: J2kCodestream = crate::parse_codestream(bytes)?;
    decode_codestream_impl(bytes, &cs, 0, max_layers)
}

/// [`decode_j2k`] against an already-parsed [`J2kCodestream`] (the
/// `bytes` must be the same buffer the codestream was parsed from).
pub fn decode_codestream(bytes: &[u8], cs: &J2kCodestream) -> Result<DecodedImage, Error> {
    decode_codestream_impl(bytes, cs, 0, u16::MAX)
}

/// Ceiling division of `v` by `2^d` (Equation B-14's reduced-grid
/// mapping; `d` is bounded by the Table A.15 `NL ≤ 32` range).
#[inline]
fn ceil_shift(v: u32, d: u8) -> u32 {
    if d == 0 {
        return v;
    }
    let step = 1u64 << d;
    ((v as u64 + step - 1) >> d) as u32
}

fn decode_codestream_impl(
    bytes: &[u8],
    cs: &J2kCodestream,
    discard_levels: u8,
    max_layers: u16,
) -> Result<DecodedImage, Error> {
    reject_unsupported_main_header_markers(bytes, cs.header.bytes_consumed)?;

    let siz = &cs.header.siz;
    let cod = &cs.header.cod;
    let qcd = &cs.header.qcd;

    // §A.6.5: resolve per-component quantisation, applying any
    // main-header QCC over the main QCD for the components it targets.
    let csiz = siz.components.len() as u16;
    let main_qccs = crate::collect_main_header_qcc(bytes, cs.header.bytes_consumed, csiz)?;
    let comp_quant = resolve_component_quant(siz.components.len(), qcd, &main_qccs)?;

    // §A.6.2: resolve per-component coding style, applying any
    // main-header COC over the main COD for the components it targets
    // (NL / code-block size / precincts / kernel per component).
    let main_cocs = crate::collect_main_header_coc(bytes, cs.header.bytes_consumed, csiz)?;
    let comp_coding = resolve_component_coding(siz.components.len(), cod, &main_cocs)?;

    // §A.6.3 / §H.1: resolve the per-component region-of-interest
    // Maxshift scaling value `s` from any main-header `RGN`. Components
    // with no `RGN` get `s = 0` (no ROI; the de-scaling is a no-op).
    let main_rgns = crate::collect_main_header_rgn(bytes, cs.header.bytes_consumed, csiz)?;
    let roi_shift = resolve_component_roi_shift(siz.components.len(), &main_rgns)?;

    // §A.6.6: a main-header POC, if present, overrides the COD/COC
    // default progression order for every tile that does not itself
    // carry a tile-part POC.
    let main_poc = crate::collect_main_header_poc(bytes, cs.header.bytes_consumed, csiz)?;

    // §A.7.4: a main-header `PPM` relocates *every* tile's packet headers
    // into the main header, one `(Nppm, Ippm)` entry per tile-part in
    // codestream order. When present, no in-stream packet headers and no
    // `PPT` may appear (a `PPT` alongside `PPM` is malformed). The
    // per-tile-part relocated buffers are mapped onto the tile-parts by
    // their codestream ordinal below.
    let main_ppm = crate::collect_main_header_ppm(bytes, cs.header.bytes_consumed)?;
    if main_ppm.is_some() {
        for tp in &cs.tile_parts {
            if tp
                .markers
                .iter()
                .any(|m| matches!(m, crate::TilePartMarker::Ppt(_)))
            {
                // §A.7.4 / §A.7.5: PPM and PPT are mutually exclusive.
                return Err(Error::InvalidMarkerLength);
            }
        }
    }
    // Map each tile-part to its codestream ordinal (the PPM series is
    // indexed by this ordinal).
    let mut tp_ordinal: BTreeMap<(u16, u8), usize> = BTreeMap::new();
    for (i, tp) in cs.tile_parts.iter().enumerate() {
        tp_ordinal.insert((tp.sot.tile_index, tp.sot.tile_part_index), i);
    }

    // The §D.6 bypass rejection and the §D.4.2 / §C.3.6 style decisions
    // are folded into `coding_params_from_cod`, which is re-run per tile
    // when a tile-part `COD` override changes the global style.
    let params = coding_params_from_cod(cod)?;

    // -- Image-area planes --
    let areas = image_area(siz)?;
    let (num_x, num_y) = tile_grid_extent(siz)?;
    let num_tiles = (num_x as u64) * (num_y as u64);

    let mut components: Vec<DecodedComponent> = Vec::with_capacity(siz.components.len());
    for (sc, area) in siz.components.iter().zip(areas.iter()) {
        // Reduced-resolution output grid: Equation B-14's ceiling
        // division maps the component's image-area corners onto the
        // kept resolution level (identity at discard_levels == 0).
        let rw = ceil_shift(area.x1, discard_levels) - ceil_shift(area.x0, discard_levels);
        let rh = ceil_shift(area.y1, discard_levels) - ceil_shift(area.y0, discard_levels);
        let len = (rw as usize)
            .checked_mul(rh as usize)
            .ok_or(Error::InvalidMarkerLength)?;
        components.push(DecodedComponent {
            width: rw,
            height: rh,
            precision_bits: sc.precision_bits,
            is_signed: sc.is_signed,
            h_separation: sc.h_separation,
            v_separation: sc.v_separation,
            samples: vec![0i32; len],
        });
    }

    // -- Group tile-parts by tile --
    //
    // §A.4.2: "The tile-parts of a given tile shall appear in order
    // (see TPsot) in the codestream. However, tile-parts from other
    // tiles may be interleaved in the codestream." Grouping by tile
    // while preserving codestream order therefore reassembles each
    // tile's ascending-TPsot chain whatever the interleaving; the
    // ordering rule itself is enforced below.
    let mut parts_by_tile: BTreeMap<u16, Vec<&crate::TilePart>> = BTreeMap::new();
    for tp in &cs.tile_parts {
        parts_by_tile.entry(tp.sot.tile_index).or_default().push(tp);
    }

    for (tile_index, parts) in parts_by_tile {
        if (tile_index as u64) >= num_tiles {
            return Err(Error::InvalidTilePartIndex);
        }
        // §A.4.2 / Table A.5: TPsot "denotes the order from 0" and the
        // tile's tile-parts shall appear in the codestream in that
        // order — so in codestream order the tile's TPsot values must
        // be exactly 0, 1, 2, … A gap, duplicate or out-of-order index
        // means a tile-part was lost or the stream was mis-assembled;
        // sorting and decoding anyway would silently mis-place packets,
        // so the fault is rejected. Table A.6: a non-zero TNsot must
        // state the tile's true tile-part count.
        for (i, tp) in parts.iter().enumerate() {
            if usize::from(tp.sot.tile_part_index) != i {
                return Err(Error::InvalidTilePartIndex);
            }
            if tp.sot.num_tile_parts != 0 && usize::from(tp.sot.num_tile_parts) != parts.len() {
                return Err(Error::InvalidTilePartIndex);
            }
        }

        // §A.6.1 / §A.6.2 / §A.6.4 / §A.6.5: COD / COC / QCD / QCC / RGN
        // overrides may appear only in the first tile-part (TPsot = 0).
        // A coding-style marker in a later tile-part of the same tile is
        // malformed — reject it before resolving so a stray override is
        // never silently dropped.
        for tp in &parts {
            if tp.sot.tile_part_index != 0 {
                for m in &tp.markers {
                    match m {
                        crate::TilePartMarker::Cod(_)
                        | crate::TilePartMarker::Coc(_)
                        | crate::TilePartMarker::Qcd(_)
                        | crate::TilePartMarker::Qcc(_)
                        | crate::TilePartMarker::Rgn(_)
                        | crate::TilePartMarker::Poc(_) => {
                            return Err(Error::InvalidMarkerLength);
                        }
                        // PLT / PPT / COM may appear in any tile-part.
                        crate::TilePartMarker::Plt(_)
                        | crate::TilePartMarker::Ppt(_)
                        | crate::TilePartMarker::Com(_) => {}
                    }
                }
            }
        }

        // §A.6 precedence: layer this tile's TPsot = 0 overrides on the
        // resolved main-header parameters. With no overrides this is the
        // main-header resolution verbatim.
        let first_markers: &[crate::TilePartMarker] =
            parts.first().map(|tp| tp.markers.as_slice()).unwrap_or(&[]);
        let tile_coding = resolve_tile_coding(
            siz.components.len(),
            &main_rgns,
            main_poc.as_ref(),
            &params,
            &comp_coding,
            &comp_quant,
            &roi_shift,
            first_markers,
        )?;

        let mut body = Vec::new();
        for tp in &parts {
            let end = tp
                .body_offset
                .checked_add(tp.body_len)
                .ok_or(Error::PsotOverflow)?;
            let slice = bytes.get(tp.body_offset..end).ok_or(Error::PsotOverflow)?;
            body.extend_from_slice(slice);
        }

        // §A.7.4 / §A.7.5: resolve the relocated packet-header buffer for
        // this tile, if any.
        //
        // * `PPM` (main header) — concatenate the per-tile-part buffers
        //   the `collect_main_header_ppm` series assigned to this tile's
        //   tile-parts, in TPsot order (the same order `parts` is sorted
        //   in). Each tile-part's buffer is addressed by its codestream
        //   ordinal.
        // * `PPT` (tile-part headers) — gather the `Ippt` payloads in
        //   increasing `Zppt` order across the tile's tile-parts. Within a
        //   tile-part the parser preserves codestream order and the
        //   tile-parts are already TPsot-sorted, so the relocated header
        //   stream is assembled in packet-decode order; the `Zppt` indices
        //   must form the contiguous run `0..N` exactly once each — a gap
        //   or duplicate signals a lost or mis-ordered PPT segment.
        let relocated: Option<Vec<u8>> = if let Some(ppm) = &main_ppm {
            let mut buf = Vec::new();
            for tp in &parts {
                let ord = *tp_ordinal
                    .get(&(tp.sot.tile_index, tp.sot.tile_part_index))
                    .ok_or(Error::InvalidTilePartIndex)?;
                let entry = ppm.get(ord).ok_or(Error::InvalidMarkerLength)?;
                buf.extend_from_slice(entry);
            }
            Some(buf)
        } else {
            gather_ppt_headers(&parts)?
        };

        let tile_planes = decode_tile(
            siz,
            &tile_coding.params,
            &tile_coding.comp_coding,
            &tile_coding.comp_quant,
            &tile_coding.roi_shift,
            &tile_coding.poc_volumes,
            tile_index as u32,
            &body,
            relocated.as_deref(),
            discard_levels,
            max_layers,
        )?;

        // Place each tile-component plane into its image-area plane at
        // the Equation B-12 offset.
        let tile = derive_tile_geometry(siz, tile_index as u32)?;
        for (c, plane) in tile_planes.iter().enumerate() {
            let tc = &tile.components[c];
            // Reduced tile-component region: the same Equation B-14
            // ceiling division that shaped the output grids (identity
            // at discard_levels == 0). Adjacent tiles stay adjacent
            // under ceil-division, so the reduced planes tile the
            // reduced image area gap-free.
            let (rx0, ry0) = (
                ceil_shift(tc.tcx0, discard_levels),
                ceil_shift(tc.tcy0, discard_levels),
            );
            let (rx1, ry1) = (
                ceil_shift(tc.tcx1, discard_levels),
                ceil_shift(tc.tcy1, discard_levels),
            );
            let (tw, th) = ((rx1 - rx0) as usize, (ry1 - ry0) as usize);
            if tw == 0 || th == 0 {
                continue;
            }
            if plane.len() != tw * th {
                return Err(Error::InvalidMarkerLength);
            }
            let comp = &mut components[c];
            let area = &areas[c];
            let dx = rx0
                .checked_sub(ceil_shift(area.x0, discard_levels))
                .ok_or(Error::InvalidMarkerLength)? as usize;
            let dy = ry0
                .checked_sub(ceil_shift(area.y0, discard_levels))
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
        width: ceil_shift(siz.x_size, discard_levels)
            .saturating_sub(ceil_shift(siz.x_offset, discard_levels)),
        height: ceil_shift(siz.y_size, discard_levels)
            .saturating_sub(ceil_shift(siz.y_offset, discard_levels)),
        components,
    })
}

/// Rebuilds a single-tile / single-tile-part J2K codestream so that the
/// tile's packet headers are **relocated** out of the bit stream into a
/// `PPT` marker segment (T.800 §A.7.5) — a clean-room transcoder used to
/// exercise the relocated-header decode path end-to-end against a real
/// fixture.
///
/// The input must have exactly one tile-part, carry no `SOP` / `EPH`
/// framing, and not already use `PPM` / `PPT`. For each packet the
/// in-stream header bytes are split from the data bytes; the headers are
/// concatenated into the `Ippt` payload of a fresh `PPT` segment
/// inserted just before the tile-part's `SOD`, and the body is rewritten
/// to hold only the concatenated packet data. `Psot` is corrected for
/// the new tile-part length.
///
/// Test-only (gated behind `cfg(test)`); not part of the public API.
#[cfg(test)]
pub(crate) fn relocate_single_tilepart_to_ppt(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let cs = crate::parse_codestream(bytes)?;
    if cs.tile_parts.len() != 1 {
        return Err(Error::NotImplemented);
    }
    let tp = &cs.tile_parts[0];

    let siz = &cs.header.siz;
    let cod = &cs.header.cod;
    let qcd = &cs.header.qcd;
    let csiz = siz.components.len() as u16;
    let main_qccs = crate::collect_main_header_qcc(bytes, cs.header.bytes_consumed, csiz)?;
    let comp_quant = resolve_component_quant(siz.components.len(), qcd, &main_qccs)?;
    let main_cocs = crate::collect_main_header_coc(bytes, cs.header.bytes_consumed, csiz)?;
    let comp_coding = resolve_component_coding(siz.components.len(), cod, &main_cocs)?;
    let main_poc = crate::collect_main_header_poc(bytes, cs.header.bytes_consumed, csiz)?;
    let main_rgns = crate::collect_main_header_rgn(bytes, cs.header.bytes_consumed, csiz)?;
    let roi_shift = resolve_component_roi_shift(siz.components.len(), &main_rgns)?;
    let params = coding_params_from_cod(cod)?;

    let tile_coding = resolve_tile_coding(
        siz.components.len(),
        &main_rgns,
        main_poc.as_ref(),
        &params,
        &comp_coding,
        &comp_quant,
        &roi_shift,
        &tp.markers,
    )?;

    // The transcoder only handles the un-framed in-stream case.
    if !matches!(tile_coding.params.sop_eph, crate::packet::SopEphMode::None) {
        return Err(Error::NotImplemented);
    }

    let body = bytes
        .get(tp.body_offset..tp.body_offset + tp.body_len)
        .ok_or(Error::PsotOverflow)?;

    // Build the tile's packet plan, walk the in-stream headers, and split
    // each packet's header bytes from its data bytes.
    let plan = build_tile_packet_plan(
        siz,
        &tile_coding.params,
        &tile_coding.comp_coding,
        &tile_coding.poc_volumes,
        tp.sot.tile_index as u32,
    )?;
    let headers = walk_tile_packet_headers(body, &plan.packets, tile_coding.params.sop_eph, None)?;

    let mut header_buf: Vec<u8> = Vec::new();
    let mut data_buf: Vec<u8> = Vec::new();
    for (header, data_offset) in &headers {
        // Inline header bytes occupy `[packet_start, data_offset)`. With
        // no SOP framing `packet_start` is the previous packet's data end,
        // which equals the running `header_buf`-relative bookkeeping — but
        // we recover the exact header span from the body directly: the
        // header is `bytes_consumed` long ending at `data_offset`.
        let head_start = data_offset
            .checked_sub(header.bytes_consumed)
            .ok_or(Error::PacketHeaderOverrun)?;
        header_buf.extend_from_slice(&body[head_start..*data_offset]);
        let data_len =
            usize::try_from(header.total_body_bytes()).map_err(|_| Error::PacketHeaderOverrun)?;
        let end = data_offset
            .checked_add(data_len)
            .ok_or(Error::PacketHeaderOverrun)?;
        data_buf.extend_from_slice(
            body.get(*data_offset..end)
                .ok_or(Error::PacketHeaderOverrun)?,
        );
    }

    // Build a PPT segment: Lppt = 2 (length) + 1 (Zppt) + Ippt.
    if header_buf.len() + 3 > u16::MAX as usize {
        return Err(Error::NotImplemented);
    }
    let mut ppt = Vec::new();
    ppt.extend_from_slice(&crate::MARKER_PPT.to_be_bytes());
    ppt.extend_from_slice(&((header_buf.len() + 3) as u16).to_be_bytes());
    ppt.push(0u8); // Zppt = 0
    ppt.extend_from_slice(&header_buf);

    // Reassemble: [.. up to SOD) + PPT + SOD + data-only body.
    // The tile-part header runs from `sot_offset` to `sod_offset`; insert
    // the PPT just before the 2-byte SOD marker.
    let pre_sod = bytes.get(..tp.sod_offset).ok_or(Error::PsotOverflow)?;
    let sod = bytes
        .get(tp.sod_offset..tp.sod_offset + 2)
        .ok_or(Error::PsotOverflow)?;
    let post_body = bytes
        .get(tp.body_offset + tp.body_len..)
        .ok_or(Error::PsotOverflow)?;

    let mut out = Vec::new();
    out.extend_from_slice(pre_sod);
    out.extend_from_slice(&ppt);
    out.extend_from_slice(sod);
    out.extend_from_slice(&data_buf);
    out.extend_from_slice(post_body);

    // Correct Psot: bytes from the SOT marker to the end of the tile-part
    // data. New length = old (sot_offset..sod_offset) header + PPT + 2
    // (SOD) + data_buf.
    let new_psot = (tp.sod_offset - tp.sot_offset) + ppt.len() + 2 + data_buf.len();
    let new_psot = u32::try_from(new_psot).map_err(|_| Error::PsotOverflow)?;
    // Psot lives at sot_offset + 2 (marker) + 2 (Lsot) + 2 (Isot) = +6.
    let psot_at = tp.sot_offset + 6;
    out[psot_at..psot_at + 4].copy_from_slice(&new_psot.to_be_bytes());

    Ok(out)
}

/// Like [`relocate_single_tilepart_to_ppt`] but relocates the headers
/// into a main-header `PPM` segment (T.800 §A.7.4) instead of a tile-part
/// `PPT`. The single tile-part's headers become the `(Nppm, Ippm)` entry
/// for tile-part 0, inserted just before the first `SOT`.
///
/// Test-only; not part of the public API.
#[cfg(test)]
pub(crate) fn relocate_single_tilepart_to_ppm(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    // Reuse the PPT transcoder to obtain the split header / data buffers,
    // then re-shape them into a PPM layout. We re-derive the split here to
    // avoid coupling the two output formats.
    let cs = crate::parse_codestream(bytes)?;
    if cs.tile_parts.len() != 1 {
        return Err(Error::NotImplemented);
    }
    let tp = &cs.tile_parts[0];

    let siz = &cs.header.siz;
    let cod = &cs.header.cod;
    let qcd = &cs.header.qcd;
    let csiz = siz.components.len() as u16;
    let main_qccs = crate::collect_main_header_qcc(bytes, cs.header.bytes_consumed, csiz)?;
    let comp_quant = resolve_component_quant(siz.components.len(), qcd, &main_qccs)?;
    let main_cocs = crate::collect_main_header_coc(bytes, cs.header.bytes_consumed, csiz)?;
    let comp_coding = resolve_component_coding(siz.components.len(), cod, &main_cocs)?;
    let main_poc = crate::collect_main_header_poc(bytes, cs.header.bytes_consumed, csiz)?;
    let main_rgns = crate::collect_main_header_rgn(bytes, cs.header.bytes_consumed, csiz)?;
    let roi_shift = resolve_component_roi_shift(siz.components.len(), &main_rgns)?;
    let params = coding_params_from_cod(cod)?;
    let tile_coding = resolve_tile_coding(
        siz.components.len(),
        &main_rgns,
        main_poc.as_ref(),
        &params,
        &comp_coding,
        &comp_quant,
        &roi_shift,
        &tp.markers,
    )?;
    if !matches!(tile_coding.params.sop_eph, crate::packet::SopEphMode::None) {
        return Err(Error::NotImplemented);
    }
    let body = bytes
        .get(tp.body_offset..tp.body_offset + tp.body_len)
        .ok_or(Error::PsotOverflow)?;
    let plan = build_tile_packet_plan(
        siz,
        &tile_coding.params,
        &tile_coding.comp_coding,
        &tile_coding.poc_volumes,
        tp.sot.tile_index as u32,
    )?;
    let headers = walk_tile_packet_headers(body, &plan.packets, tile_coding.params.sop_eph, None)?;
    let mut header_buf: Vec<u8> = Vec::new();
    let mut data_buf: Vec<u8> = Vec::new();
    for (header, data_offset) in &headers {
        let head_start = data_offset
            .checked_sub(header.bytes_consumed)
            .ok_or(Error::PacketHeaderOverrun)?;
        header_buf.extend_from_slice(&body[head_start..*data_offset]);
        let data_len =
            usize::try_from(header.total_body_bytes()).map_err(|_| Error::PacketHeaderOverrun)?;
        let end = data_offset
            .checked_add(data_len)
            .ok_or(Error::PacketHeaderOverrun)?;
        data_buf.extend_from_slice(
            body.get(*data_offset..end)
                .ok_or(Error::PacketHeaderOverrun)?,
        );
    }

    // PPM Ippm payload for the single tile-part: Nppm (u32) + Ippm.
    let mut ppm_payload = Vec::new();
    ppm_payload.extend_from_slice(&(header_buf.len() as u32).to_be_bytes());
    ppm_payload.extend_from_slice(&header_buf);
    // Lppm = 2 (length) + 1 (Zppm) + payload.
    if ppm_payload.len() + 3 > u16::MAX as usize {
        return Err(Error::NotImplemented);
    }
    let mut ppm = Vec::new();
    ppm.extend_from_slice(&crate::MARKER_PPM.to_be_bytes());
    ppm.extend_from_slice(&((ppm_payload.len() + 3) as u16).to_be_bytes());
    ppm.push(0u8); // Zppm = 0
    ppm.extend_from_slice(&ppm_payload);

    // Reassemble: [.. up to first SOT) + PPM + [SOT .. SOD] + SOD +
    // data-only body. The PPM is a main-header marker, inserted before the
    // first SOT.
    let pre_sot = bytes.get(..tp.sot_offset).ok_or(Error::PsotOverflow)?;
    let sot_to_sod = bytes
        .get(tp.sot_offset..tp.sod_offset + 2)
        .ok_or(Error::PsotOverflow)?;
    let post_body = bytes
        .get(tp.body_offset + tp.body_len..)
        .ok_or(Error::PsotOverflow)?;

    let mut out = Vec::new();
    out.extend_from_slice(pre_sot);
    out.extend_from_slice(&ppm);
    out.extend_from_slice(sot_to_sod);
    out.extend_from_slice(&data_buf);
    out.extend_from_slice(post_body);

    // Correct Psot (does NOT include the main-header PPM): old header span
    // + 2 (SOD) + data_buf.
    let new_psot = (tp.sod_offset - tp.sot_offset) + 2 + data_buf.len();
    let new_psot = u32::try_from(new_psot).map_err(|_| Error::PsotOverflow)?;
    // The SOT moved forward by the inserted PPM length.
    let psot_at = tp.sot_offset + ppm.len() + 6;
    out[psot_at..psot_at + 4].copy_from_slice(&new_psot.to_be_bytes());

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tile-part carrying the given `(Zppt, payload)` PPT
    /// segments (plus, optionally, a leading COM so the PPTs are not the
    /// only markers). Offsets are dummies — `gather_ppt_headers` only
    /// reads the markers.
    fn tile_part_with_ppts(tile_part_index: u8, ppts: &[(u8, &[u8])]) -> crate::TilePart {
        let markers = ppts
            .iter()
            .map(|(z, b)| {
                crate::TilePartMarker::Ppt(crate::Ppt {
                    z_index: *z,
                    packet_headers: b.to_vec(),
                })
            })
            .collect();
        crate::TilePart {
            sot: crate::Sot {
                tile_index: 0,
                psot: 0,
                tile_part_index,
                num_tile_parts: 0,
            },
            sot_offset: 0,
            sod_offset: 0,
            body_offset: 0,
            body_len: 0,
            markers,
        }
    }

    /// No PPT in any tile-part → `None` (the §B.10 in-stream path).
    #[test]
    fn gather_ppt_none_without_ppt() {
        let tp = tile_part_with_ppts(0, &[]);
        let parts = [&tp];
        assert_eq!(gather_ppt_headers(&parts).unwrap(), None);
    }

    /// PPTs across two tile-parts are concatenated in increasing Zppt
    /// order (§A.7.5), independent of the codestream order they appear.
    #[test]
    fn gather_ppt_concatenates_in_zppt_order() {
        // tp0 carries Zppt=1, tp1 carries Zppt=0 — the gather must emit
        // segment 0's bytes first regardless.
        let tp0 = tile_part_with_ppts(0, &[(1u8, &[0xCC, 0xDD][..])]);
        let tp1 = tile_part_with_ppts(1, &[(0u8, &[0xAA, 0xBB][..])]);
        let parts = [&tp0, &tp1];
        let got = gather_ppt_headers(&parts).unwrap().unwrap();
        assert_eq!(got, vec![0xAA, 0xBB, 0xCC, 0xDD]);
    }

    /// Multiple PPTs in one tile-part header are ordered by Zppt too.
    #[test]
    fn gather_ppt_multiple_in_one_header() {
        let tp = tile_part_with_ppts(
            0,
            &[(2u8, &[0x33][..]), (0u8, &[0x11][..]), (1u8, &[0x22][..])],
        );
        let parts = [&tp];
        let got = gather_ppt_headers(&parts).unwrap().unwrap();
        assert_eq!(got, vec![0x11, 0x22, 0x33]);
    }

    /// A gap in the Zppt run (0, 2 with no 1) is a lost relocated-header
    /// segment — rejected.
    #[test]
    fn gather_ppt_rejects_zppt_gap() {
        let tp = tile_part_with_ppts(0, &[(0u8, &[0x11][..]), (2u8, &[0x22][..])]);
        let parts = [&tp];
        assert!(matches!(
            gather_ppt_headers(&parts),
            Err(Error::InvalidMarkerLength)
        ));
    }

    /// A duplicated Zppt is rejected (each index must appear once).
    #[test]
    fn gather_ppt_rejects_zppt_duplicate() {
        let tp = tile_part_with_ppts(0, &[(0u8, &[0x11][..]), (0u8, &[0x22][..])]);
        let parts = [&tp];
        assert!(matches!(
            gather_ppt_headers(&parts),
            Err(Error::InvalidMarkerLength)
        ));
    }

    // -- End-to-end PPT relocation against a real fixture --------------

    /// A real single-tile-part fixture, transcoded so its packet headers
    /// move into a `PPT` segment, must decode **pixel-identically** to the
    /// in-stream original — proving the full relocated-header decode path
    /// (gather → separate walk → tier-1) reconstructs the same image.
    #[test]
    fn ppt_relocated_gray_53_matches_inline() {
        const GRAY_53: &[u8] = include_bytes!("../tests/data/gray-17x13-53.j2k");
        let original = decode_j2k(GRAY_53).expect("decode in-stream original");

        let relocated = relocate_single_tilepart_to_ppt(GRAY_53).expect("relocate to PPT");

        // The transcoded stream genuinely carries a PPT now.
        let cs = crate::parse_codestream(&relocated).expect("parse relocated");
        assert!(cs.tile_parts[0]
            .markers
            .iter()
            .any(|m| matches!(m, crate::TilePartMarker::Ppt(_))));

        let decoded = decode_j2k(&relocated).expect("decode relocated PPT stream");
        assert_eq!(decoded.components.len(), original.components.len());
        for (a, b) in decoded.components.iter().zip(original.components.iter()) {
            assert_eq!(a.samples, b.samples, "PPT-relocated decode diverged");
        }
    }

    /// The same fixture's 9-7 irreversible multi-resolution sibling, to
    /// exercise relocation over multiple packets (NL > 0) and the lossy
    /// path.
    #[test]
    fn ppt_relocated_gray_97_matches_inline() {
        const GRAY_97: &[u8] = include_bytes!("../tests/data/gray-32x32-97full.j2k");
        let original = decode_j2k(GRAY_97).expect("decode in-stream original");
        let relocated = relocate_single_tilepart_to_ppt(GRAY_97).expect("relocate to PPT");
        let decoded = decode_j2k(&relocated).expect("decode relocated PPT stream");
        assert_eq!(decoded.components.len(), original.components.len());
        for (a, b) in decoded.components.iter().zip(original.components.iter()) {
            assert_eq!(a.samples, b.samples, "PPT-relocated 9-7 decode diverged");
        }
    }

    /// The same fixtures relocated into a main-header `PPM` must also
    /// decode pixel-identically, proving the §A.7.4 gather + per-tile-part
    /// `(Nppm, Ippm)` split + separate-walk path.
    #[test]
    fn ppm_relocated_gray_53_matches_inline() {
        const GRAY_53: &[u8] = include_bytes!("../tests/data/gray-17x13-53.j2k");
        let original = decode_j2k(GRAY_53).expect("decode in-stream original");
        let relocated = relocate_single_tilepart_to_ppm(GRAY_53).expect("relocate to PPM");
        let decoded = decode_j2k(&relocated).expect("decode relocated PPM stream");
        for (a, b) in decoded.components.iter().zip(original.components.iter()) {
            assert_eq!(a.samples, b.samples, "PPM-relocated decode diverged");
        }
    }

    #[test]
    fn ppm_relocated_gray_97_matches_inline() {
        const GRAY_97: &[u8] = include_bytes!("../tests/data/gray-32x32-97full.j2k");
        let original = decode_j2k(GRAY_97).expect("decode in-stream original");
        let relocated = relocate_single_tilepart_to_ppm(GRAY_97).expect("relocate to PPM");
        let decoded = decode_j2k(&relocated).expect("decode relocated PPM stream");
        for (a, b) in decoded.components.iter().zip(original.components.iter()) {
            assert_eq!(a.samples, b.samples, "PPM-relocated 9-7 decode diverged");
        }
    }

    /// Broader single-tile-part fixtures (multi-precinct, multi-layer,
    /// RGB / RCT) relocated into both `PPT` and `PPM` must decode
    /// pixel-identically — exercising many-packets-per-tile-part
    /// relocation across multiple precincts, layers and components.
    #[test]
    fn relocated_broad_fixtures_match_inline() {
        const MULTIPRECINCT: &[u8] =
            include_bytes!("../tests/data/gray-40x40-multiprecinct-53.j2k");
        const MULTILAYER: &[u8] = include_bytes!("../tests/data/gray-64x64-multilayer-53.j2k");
        const RGB_RCT: &[u8] = include_bytes!("../tests/data/rgb-16x16-rct-53.j2k");

        for (name, fixture) in [
            ("multiprecinct", MULTIPRECINCT),
            ("multilayer", MULTILAYER),
            ("rgb-rct", RGB_RCT),
        ] {
            let original = decode_j2k(fixture).expect("decode original");
            for (kind, relocated) in [
                (
                    "PPT",
                    relocate_single_tilepart_to_ppt(fixture).expect("relocate PPT"),
                ),
                (
                    "PPM",
                    relocate_single_tilepart_to_ppm(fixture).expect("relocate PPM"),
                ),
            ] {
                let decoded = decode_j2k(&relocated).expect("decode relocated");
                assert_eq!(
                    decoded.components.len(),
                    original.components.len(),
                    "{name}/{kind} component count"
                );
                for (a, b) in decoded.components.iter().zip(original.components.iter()) {
                    assert_eq!(
                        a.samples, b.samples,
                        "{name}/{kind} relocated decode diverged"
                    );
                }
            }
        }
    }

    /// A stream carrying *both* a main-header `PPM` and a tile-part `PPT`
    /// is malformed (§A.7.4 mutual exclusion) — the decoder rejects it.
    #[test]
    fn ppm_with_ppt_is_rejected() {
        const GRAY_53: &[u8] = include_bytes!("../tests/data/gray-17x13-53.j2k");
        // Relocate to PPM, then also splice a (harmless, empty-ish) PPT
        // into the tile-part header to violate the mutual-exclusion rule.
        let ppm_stream = relocate_single_tilepart_to_ppm(GRAY_53).expect("relocate to PPM");
        let cs = crate::parse_codestream(&ppm_stream).expect("parse ppm stream");
        let tp = &cs.tile_parts[0];
        // Insert a 4-byte PPT (Lppt=3: Zppt + 0 Ippt bytes is invalid;
        // use 1 Ippt byte → Lppt=4) just before SOD.
        let mut ppt = Vec::new();
        ppt.extend_from_slice(&crate::MARKER_PPT.to_be_bytes());
        ppt.extend_from_slice(&4u16.to_be_bytes());
        ppt.push(0u8); // Zppt
        ppt.push(0u8); // 1 Ippt byte
        let mut spliced = Vec::new();
        spliced.extend_from_slice(&ppm_stream[..tp.sod_offset]);
        spliced.extend_from_slice(&ppt);
        spliced.extend_from_slice(&ppm_stream[tp.sod_offset..]);
        // Fix Psot for the inserted PPT bytes so parsing reaches decode
        // (the PPM transcoder always writes a concrete, non-zero Psot).
        let new_psot = tp.sot.psot + ppt.len() as u32;
        let psot_at = tp.sot_offset + 6;
        spliced[psot_at..psot_at + 4].copy_from_slice(&new_psot.to_be_bytes());
        // Decode must reject the PPM+PPT combination.
        assert!(matches!(
            decode_j2k(&spliced),
            Err(Error::InvalidMarkerLength)
        ));
    }

    /// Build a minimal main-header byte span `SOC | SIZ-stub | CAP` for
    /// the CAP-accept tests. `pcap` is the 32-bit Pcap field.
    fn header_with_cap(pcap: u32) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(&MARKER_SOC.to_be_bytes());
        // A SIZ-shaped filler segment (marker + length(4) + 0 payload) so
        // the walker length-skips past it.
        h.extend_from_slice(&MARKER_SIZ.to_be_bytes());
        h.extend_from_slice(&4u16.to_be_bytes());
        h.extend_from_slice(&[0u8, 0u8]);
        // CAP: marker + Lcap(6) + Pcap(4) + Ccap15(2).
        h.extend_from_slice(&MARKER_CAP.to_be_bytes());
        h.extend_from_slice(&8u16.to_be_bytes());
        h.extend_from_slice(&pcap.to_be_bytes());
        h.extend_from_slice(&[0u8, 0u8]); // Ccap15
        h
    }

    /// A CAP segment signalling only HTJ2K (Pcap bit 15 set) is accepted.
    #[test]
    fn cap_htj2k_only_accepted() {
        let pcap = 1u32 << (32 - 15); // Pcap15 set, all else 0
        let h = header_with_cap(pcap);
        let end = h.len();
        assert!(reject_unsupported_main_header_markers(&h, end).is_ok());
    }

    /// A CAP segment with Pcap15 clear, or with any other capability bit
    /// set, is rejected as NotImplemented.
    #[test]
    fn cap_non_htj2k_rejected() {
        // Pcap15 clear.
        let h = header_with_cap(0);
        let end = h.len();
        assert!(matches!(
            reject_unsupported_main_header_markers(&h, end),
            Err(Error::NotImplemented)
        ));
        // Pcap15 set but also Pcap2 (some other part) set.
        let pcap = (1u32 << (32 - 15)) | (1u32 << (32 - 2));
        let h2 = header_with_cap(pcap);
        let end2 = h2.len();
        assert!(matches!(
            reject_unsupported_main_header_markers(&h2, end2),
            Err(Error::NotImplemented)
        ));
    }

    /// SPcod bit 6 maps to `CodeBlockStyle::high_throughput` and, when
    /// set, the coding-params builder forces the Annex D bypass /
    /// termination flags off (T.814 §A.4 Table A.4).
    #[test]
    fn ht_style_forces_annex_d_flags_off() {
        // bit 6 (HT) + bit 0 (bypass) + bit 2 (termination) all set.
        let style = crate::CodeBlockStyle::from_byte(0x45);
        assert!(style.high_throughput());
        assert!(style.selective_arithmetic_coding_bypass());
        assert!(style.termination_on_each_coding_pass());
    }

    #[test]
    fn predictable_termination_bit_is_read_into_block_style() {
        // Table A.19 bit 4 (0x10) set in the style byte surfaces as
        // BlockStyle::predictable_termination so the tier-1 driver runs
        // the §D.4.2 conformance check.
        assert!(BlockStyle::from_style_byte(0x10).predictable_termination);
        assert!(!BlockStyle::from_style_byte(0x00).predictable_termination);
    }

    #[test]
    fn ht_forces_predictable_termination_off() {
        // T.814 Table A.13: predictable termination "does not apply to HT
        // code-blocks". bit 6 (HT) + bit 4 (predictable termination) →
        // the builder forces predictable termination off.
        let style = BlockStyle::from_style_byte(0x50);
        assert!(style.high_throughput);
        assert!(!style.predictable_termination);
    }

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

    // -- §A.6.2 COC per-component coding-style override --

    fn cod(
        n_l: u8,
        cb_w: u8,
        cb_h: u8,
        style: u8,
        transform: WaveletTransform,
        precincts: &[u8],
    ) -> crate::Cod {
        crate::Cod {
            scod: u8::from(!precincts.is_empty()),
            user_defined_precincts: !precincts.is_empty(),
            sop_marker_allowed: false,
            eph_marker_used: false,
            progression: ProgressionOrder::Lrcp,
            layers: 1,
            multi_component_transform: 0,
            decomposition_levels: n_l,
            code_block_width_exp: cb_w,
            code_block_height_exp: cb_h,
            code_block_style: style,
            transform,
            precincts: precincts.to_vec(),
        }
    }

    fn coc(
        c: u16,
        n_l: u8,
        cb_w: u8,
        cb_h: u8,
        style: u8,
        transform: WaveletTransform,
        precincts: &[u8],
    ) -> crate::Coc {
        crate::Coc {
            component_index: c,
            scoc: u8::from(!precincts.is_empty()),
            user_defined_precincts: !precincts.is_empty(),
            decomposition_levels: n_l,
            code_block_width_exp: cb_w,
            code_block_height_exp: cb_h,
            code_block_style: style,
            transform,
            precincts: precincts.to_vec(),
        }
    }

    #[test]
    fn resolve_component_coding_defaults_to_cod() {
        // §A.6.2: with no COC, every component inherits the COD style.
        let c = cod(3, 2, 2, 0, WaveletTransform::Reversible5x3, &[]);
        let resolved = resolve_component_coding(3, &c, &[]).expect("resolve");
        assert_eq!(resolved.len(), 3);
        for cc in &resolved {
            assert_eq!(cc.n_l, 3);
            // Resolved xcb/ycb add the +2 Table A.18 offset.
            assert_eq!(cc.xcb, 4);
            assert_eq!(cc.ycb, 4);
            assert_eq!(cc.transform, WaveletTransform::Reversible5x3);
            assert!(cc.precincts.is_empty());
        }
    }

    #[test]
    fn resolve_component_coding_applies_override() {
        // §A.6.2: a COC for component 1 overrides NL / code-block size /
        // precincts / kernel only there. The style byte must match COD.
        let base = cod(5, 4, 4, 0x00, WaveletTransform::Irreversible9x7, &[]);
        let c1 = coc(
            1,
            2,
            1,
            1,
            0x00,
            WaveletTransform::Irreversible9x7,
            &[0x44, 0x33, 0x22],
        );
        let resolved =
            resolve_component_coding(3, &base, std::slice::from_ref(&c1)).expect("resolve");
        // Components 0 and 2 keep the COD default (xcb = 4 + 2 = 6).
        assert_eq!(resolved[0].n_l, 5);
        assert_eq!(resolved[0].xcb, 6);
        assert!(resolved[2].precincts.is_empty());
        // Component 1 takes the COC values (xcb = 1 + 2 = 3).
        assert_eq!(resolved[1].n_l, 2);
        assert_eq!(resolved[1].xcb, 3);
        assert_eq!(resolved[1].ycb, 3);
        assert_eq!(resolved[1].precincts, &[0x44, 0x33, 0x22]);
    }

    #[test]
    fn resolve_component_coding_rejects_out_of_range_index() {
        let base = cod(3, 2, 2, 0, WaveletTransform::Reversible5x3, &[]);
        let bad = coc(5, 3, 2, 2, 0, WaveletTransform::Reversible5x3, &[]);
        assert!(resolve_component_coding(3, &base, std::slice::from_ref(&bad)).is_err());
    }

    #[test]
    fn resolve_component_coding_rejects_duplicate_component() {
        // §A.6.2: no more than one COC per component per header.
        let base = cod(3, 2, 2, 0, WaveletTransform::Reversible5x3, &[]);
        let a = coc(0, 3, 2, 2, 0, WaveletTransform::Reversible5x3, &[]);
        let b = coc(0, 2, 1, 1, 0, WaveletTransform::Reversible5x3, &[]);
        assert!(resolve_component_coding(2, &base, &[a, b]).is_err());
    }

    #[test]
    fn resolve_component_coding_honours_divergent_code_block_style() {
        // §A.6.2: a COC carries its own Table A.19 style byte, so the
        // per-component style (and hence the §B.10.7 segment split and
        // the tier-1 dispatch) resolves independently — component 1
        // here turns on the §D.4.2 per-pass termination while
        // component 0 keeps the COD default.
        let base = cod(3, 2, 2, 0x00, WaveletTransform::Reversible5x3, &[]);
        let diverge = coc(1, 3, 2, 2, 0x04, WaveletTransform::Reversible5x3, &[]);
        let out = resolve_component_coding(2, &base, std::slice::from_ref(&diverge)).unwrap();
        assert!(!out[0].style.termination_on_each_coding_pass);
        assert!(out[1].style.termination_on_each_coding_pass);
        assert!(matches!(out[0].style.split(), SegmentSplit::Single));
        assert!(matches!(out[1].style.split(), SegmentSplit::PerPass));
        // And the T.814 HTDECLARED shape: SPcoc bit 6 on one component
        // only.
        let ht_coc = coc(1, 3, 2, 2, 0x40, WaveletTransform::Reversible5x3, &[]);
        let out = resolve_component_coding(2, &base, std::slice::from_ref(&ht_coc)).unwrap();
        assert!(!out[0].style.high_throughput);
        assert!(out[1].style.high_throughput);
        assert!(matches!(out[1].style.split(), SegmentSplit::Ht));
    }

    // -- §A.6 tile-part header override precedence --

    fn rgn(c: u16, srgn: u8, sprgn: u8) -> crate::Rgn {
        crate::Rgn {
            component_index: c,
            srgn,
            sprgn,
        }
    }

    /// Resolve the main-header default coding for the precedence tests:
    /// the §A.6 `Tile > Main` layering starts from this.
    fn main_defaults<'a>(
        num: usize,
        c: &'a crate::Cod,
        q: &'a crate::Qcd,
    ) -> (CodingParams, Vec<ComponentCoding>, Vec<ComponentQuant<'a>>) {
        let params = coding_params_from_cod(c).expect("params");
        let coding = resolve_component_coding(num, c, &[]).expect("coding");
        let quant = resolve_component_quant(num, q, &[]).expect("quant");
        (params, coding, quant)
    }

    #[test]
    fn tile_no_overrides_keeps_main_resolution() {
        // §A.6: a tile-part with no COD/COC/QCD/QCC/RGN inherits the
        // resolved main-header parameters verbatim.
        let c = cod(2, 2, 2, 0x00, WaveletTransform::Reversible5x3, &[]);
        let q = qcd(QuantizationStyle::None, 2, &[0x40, 0x41, 0x42]);
        let (params, coding, quant) = main_defaults(2, &c, &q);
        let roi = vec![0u32; 2];
        let resolved = resolve_tile_coding(2, &[], None, &params, &coding, &quant, &roi, &[])
            .expect("resolve");
        assert_eq!(resolved.comp_coding[0].n_l, 2);
        assert_eq!(resolved.comp_quant[0].guard_bits, 2);
        assert_eq!(resolved.roi_shift, vec![0, 0]);
        assert_eq!(resolved.params.layers, 1);
    }

    #[test]
    fn tile_cod_supersedes_main_cod_for_whole_tile() {
        // §A.6.1: a tile-part COD overrides the main COD (and COCs) for
        // every component of the tile.
        let main_c = cod(2, 2, 2, 0x00, WaveletTransform::Reversible5x3, &[]);
        let q = qcd(QuantizationStyle::None, 2, &[0x40, 0x41, 0x42]);
        let (params, coding, quant) = main_defaults(2, &main_c, &q);
        let roi = vec![0u32; 2];
        // Tile COD: NL = 4, code-block 8×8 (xcb=ycb=1 → +2 = 3).
        let tile_c = cod(4, 1, 1, 0x00, WaveletTransform::Reversible5x3, &[]);
        let markers = vec![crate::TilePartMarker::Cod(tile_c)];
        let resolved = resolve_tile_coding(2, &[], None, &params, &coding, &quant, &roi, &markers)
            .expect("resolve");
        for cc in &resolved.comp_coding {
            assert_eq!(cc.n_l, 4);
            assert_eq!(cc.xcb, 3);
        }
    }

    #[test]
    fn tile_coc_outranks_tile_cod_per_component() {
        // §A.6.2 precedence: Tile COC > Tile COD. A tile COC for
        // component 1 refines the tile COD only there.
        let main_c = cod(2, 2, 2, 0x00, WaveletTransform::Reversible5x3, &[]);
        let q = qcd(QuantizationStyle::None, 2, &[0x40, 0x41, 0x42]);
        let (params, coding, quant) = main_defaults(2, &main_c, &q);
        let roi = vec![0u32; 2];
        let tile_c = cod(4, 1, 1, 0x00, WaveletTransform::Reversible5x3, &[]);
        let tile_coc1 = coc(1, 1, 2, 2, 0x00, WaveletTransform::Reversible5x3, &[]);
        let markers = vec![
            crate::TilePartMarker::Cod(tile_c),
            crate::TilePartMarker::Coc(tile_coc1),
        ];
        let resolved = resolve_tile_coding(2, &[], None, &params, &coding, &quant, &roi, &markers)
            .expect("resolve");
        // Component 0 follows the tile COD (NL = 4); component 1 the COC.
        assert_eq!(resolved.comp_coding[0].n_l, 4);
        assert_eq!(resolved.comp_coding[1].n_l, 1);
        assert_eq!(resolved.comp_coding[1].xcb, 4);
    }

    #[test]
    fn tile_coc_alone_overrides_main_per_component() {
        // §A.6.2: with no tile COD, a tile COC overrides the resolved
        // main parameters for its component (and outranks the main COC).
        let main_c = cod(3, 2, 2, 0x00, WaveletTransform::Reversible5x3, &[]);
        let q = qcd(QuantizationStyle::None, 2, &[0x40, 0x41, 0x42, 0x43]);
        let (params, coding, quant) = main_defaults(2, &main_c, &q);
        let roi = vec![0u32; 2];
        let tile_coc0 = coc(0, 1, 1, 1, 0x00, WaveletTransform::Reversible5x3, &[]);
        let markers = vec![crate::TilePartMarker::Coc(tile_coc0)];
        let resolved = resolve_tile_coding(2, &[], None, &params, &coding, &quant, &roi, &markers)
            .expect("resolve");
        assert_eq!(resolved.comp_coding[0].n_l, 1);
        assert_eq!(resolved.comp_coding[0].xcb, 3);
        // Component 1 keeps the main COD.
        assert_eq!(resolved.comp_coding[1].n_l, 3);
    }

    #[test]
    fn tile_qcd_supersedes_main_quant() {
        // §A.6.4: a tile-part QCD overrides the main QCD/QCC for every
        // component.
        let main_c = cod(2, 2, 2, 0x00, WaveletTransform::Reversible5x3, &[]);
        let main_q = qcd(QuantizationStyle::None, 2, &[0x40, 0x41, 0x42]);
        let (params, coding, quant) = main_defaults(2, &main_c, &main_q);
        let roi = vec![0u32; 2];
        let tile_q = qcd(QuantizationStyle::None, 4, &[0x50, 0x51, 0x52]);
        let markers = vec![crate::TilePartMarker::Qcd(tile_q)];
        let resolved = resolve_tile_coding(2, &[], None, &params, &coding, &quant, &roi, &markers)
            .expect("resolve");
        for cq in &resolved.comp_quant {
            assert_eq!(cq.guard_bits, 4);
            assert_eq!(cq.spqcd, &[0x50, 0x51, 0x52]);
        }
    }

    #[test]
    fn tile_qcc_outranks_tile_qcd_per_component() {
        // §A.6.5 precedence: Tile QCC > Tile QCD.
        let main_c = cod(2, 2, 2, 0x00, WaveletTransform::Reversible5x3, &[]);
        let main_q = qcd(QuantizationStyle::None, 2, &[0x40, 0x41, 0x42]);
        let (params, coding, quant) = main_defaults(2, &main_c, &main_q);
        let roi = vec![0u32; 2];
        let tile_q = qcd(QuantizationStyle::None, 4, &[0x50, 0x51, 0x52]);
        let tile_qcc1 = qcc(1, QuantizationStyle::None, 5, &[0x60, 0x61, 0x62]);
        let markers = vec![
            crate::TilePartMarker::Qcd(tile_q),
            crate::TilePartMarker::Qcc(tile_qcc1),
        ];
        let resolved = resolve_tile_coding(2, &[], None, &params, &coding, &quant, &roi, &markers)
            .expect("resolve");
        // Component 0 follows the tile QCD; component 1 the tile QCC.
        assert_eq!(resolved.comp_quant[0].guard_bits, 4);
        assert_eq!(resolved.comp_quant[1].guard_bits, 5);
        assert_eq!(resolved.comp_quant[1].spqcd, &[0x60, 0x61, 0x62]);
    }

    #[test]
    fn tile_qcc_alone_overrides_main_per_component() {
        // §A.6.5: with no tile QCD, a tile QCC overrides the resolved
        // main quant for its component only.
        let main_c = cod(2, 2, 2, 0x00, WaveletTransform::Reversible5x3, &[]);
        let main_q = qcd(QuantizationStyle::None, 2, &[0x40, 0x41, 0x42]);
        let (params, coding, quant) = main_defaults(2, &main_c, &main_q);
        let roi = vec![0u32; 2];
        let tile_qcc0 = qcc(0, QuantizationStyle::None, 6, &[0x70, 0x71, 0x72]);
        let markers = vec![crate::TilePartMarker::Qcc(tile_qcc0)];
        let resolved = resolve_tile_coding(2, &[], None, &params, &coding, &quant, &roi, &markers)
            .expect("resolve");
        assert_eq!(resolved.comp_quant[0].guard_bits, 6);
        assert_eq!(resolved.comp_quant[1].guard_bits, 2);
    }

    #[test]
    fn tile_rgn_overrides_main_roi_per_component() {
        // §A.6.3: a tile-part RGN overrides the main RGN for its
        // component; others keep the main shift.
        let main_c = cod(2, 2, 2, 0x00, WaveletTransform::Reversible5x3, &[]);
        let main_q = qcd(QuantizationStyle::None, 2, &[0x40, 0x41, 0x42]);
        let (params, coding, quant) = main_defaults(2, &main_c, &main_q);
        let main_rgns = [rgn(0, 0, 3)]; // component 0 has Maxshift s = 3.
        let roi = resolve_component_roi_shift(2, &main_rgns).expect("roi");
        let tile_rgn1 = rgn(1, 0, 7); // component 1 gets s = 7 in this tile.
        let markers = vec![crate::TilePartMarker::Rgn(tile_rgn1)];
        let resolved = resolve_tile_coding(
            2, &main_rgns, None, &params, &coding, &quant, &roi, &markers,
        )
        .expect("resolve");
        assert_eq!(resolved.roi_shift, vec![3, 7]);
    }

    #[test]
    fn tile_rgn_non_maxshift_style_is_rejected() {
        // T.800 Table A.25 (Part 1) defines only Srgn = 0 (Maxshift); a
        // tile-part RGN with a non-zero Srgn is the Part-2 scaling-based
        // arbitrary ROI, outside this Part-1 decoder. It is rejected with
        // NotImplemented (not mis-decoded) — mirroring the main-header
        // path.
        let main_c = cod(2, 2, 2, 0x00, WaveletTransform::Reversible5x3, &[]);
        let main_q = qcd(QuantizationStyle::None, 2, &[0x40, 0x41, 0x42]);
        let (params, coding, quant) = main_defaults(2, &main_c, &main_q);
        let roi = vec![0u32; 2];
        let tile_rgn = rgn(1, 1, 7); // Srgn = 1 (Part-2 rectangle ROI).
        let markers = vec![crate::TilePartMarker::Rgn(tile_rgn)];
        assert!(matches!(
            resolve_tile_coding(2, &[], None, &params, &coding, &quant, &roi, &markers),
            Err(Error::NotImplemented)
        ));
    }

    #[test]
    fn tile_cod_bypass_style_propagates_into_resolved_params() {
        // §D.6 selective arithmetic-coding bypass: a tile COD that sets
        // the Table A.19 bit-0 style flag turns the bypass schedule on
        // for that tile even though the main COD did not. The resolved
        // tile params carry the flag into the tier-1 driver.
        let main_c = cod(2, 2, 2, 0x00, WaveletTransform::Reversible5x3, &[]);
        let main_q = qcd(QuantizationStyle::None, 2, &[0x40, 0x41, 0x42]);
        let (params, coding, quant) = main_defaults(2, &main_c, &main_q);
        assert!(!coding[0].style.selective_arithmetic_coding_bypass);
        let roi = vec![0u32; 2];
        let tile_c = cod(2, 2, 2, 0x01, WaveletTransform::Reversible5x3, &[]);
        let markers = vec![crate::TilePartMarker::Cod(tile_c)];
        let resolved = resolve_tile_coding(2, &[], None, &params, &coding, &quant, &roi, &markers)
            .expect("resolve");
        for cc in &resolved.comp_coding {
            assert!(cc.style.selective_arithmetic_coding_bypass);
        }
    }

    #[test]
    fn tile_duplicate_cod_is_rejected() {
        // §A.6.1: at most one COD per tile-part header.
        let main_c = cod(2, 2, 2, 0x00, WaveletTransform::Reversible5x3, &[]);
        let main_q = qcd(QuantizationStyle::None, 2, &[0x40, 0x41, 0x42]);
        let (params, coding, quant) = main_defaults(2, &main_c, &main_q);
        let roi = vec![0u32; 2];
        let a = cod(2, 2, 2, 0x00, WaveletTransform::Reversible5x3, &[]);
        let b = cod(3, 1, 1, 0x00, WaveletTransform::Reversible5x3, &[]);
        let markers = vec![crate::TilePartMarker::Cod(a), crate::TilePartMarker::Cod(b)];
        assert!(
            resolve_tile_coding(2, &[], None, &params, &coding, &quant, &roi, &markers).is_err()
        );
    }

    /// A `PocProgression` covering the whole component/resolution/layer
    /// cube for one of the §A.6.6 orders.
    fn poc_entry(order: ProgressionOrder) -> crate::PocProgression {
        crate::PocProgression {
            resolution_start: 0,
            component_start: 0,
            layer_end: 1,
            resolution_end: 33,
            component_end: 256,
            progression: order,
        }
    }

    #[test]
    fn tile_poc_resolves_into_volumes() {
        // §A.6.6: a tile-part POC is honoured — its progressions become
        // the runtime PocVolume list driving §B.12.2 enumeration.
        let main_c = cod(2, 2, 2, 0x00, WaveletTransform::Reversible5x3, &[]);
        let main_q = qcd(QuantizationStyle::None, 2, &[0x40, 0x41, 0x42]);
        let (params, coding, quant) = main_defaults(2, &main_c, &main_q);
        let roi = vec![0u32; 2];
        let markers = vec![crate::TilePartMarker::Poc(crate::Poc {
            progressions: vec![poc_entry(ProgressionOrder::Rlcp)],
        })];
        let resolved = resolve_tile_coding(2, &[], None, &params, &coding, &quant, &roi, &markers)
            .expect("tile POC resolves");
        assert_eq!(resolved.poc_volumes.len(), 1);
        assert_eq!(resolved.poc_volumes[0].order, ProgressionOrder::Rlcp);
    }

    #[test]
    fn tile_poc_overrides_main_poc() {
        // §A.6.6 precedence: Tile-part POC > Main POC. When both are
        // present the tile-part POC wins for that tile.
        let main_c = cod(2, 2, 2, 0x00, WaveletTransform::Reversible5x3, &[]);
        let main_q = qcd(QuantizationStyle::None, 2, &[0x40, 0x41, 0x42]);
        let (params, coding, quant) = main_defaults(2, &main_c, &main_q);
        let roi = vec![0u32; 2];
        let main_poc = crate::Poc {
            progressions: vec![poc_entry(ProgressionOrder::Lrcp)],
        };
        let markers = vec![crate::TilePartMarker::Poc(crate::Poc {
            progressions: vec![poc_entry(ProgressionOrder::Cprl)],
        })];
        let resolved = resolve_tile_coding(
            2,
            &[],
            Some(&main_poc),
            &params,
            &coding,
            &quant,
            &roi,
            &markers,
        )
        .expect("resolve");
        assert_eq!(resolved.poc_volumes.len(), 1);
        // The tile-part POC (CPRL) supersedes the main POC (LRCP).
        assert_eq!(resolved.poc_volumes[0].order, ProgressionOrder::Cprl);
    }

    #[test]
    fn main_poc_applies_when_no_tile_poc() {
        // §A.6.6: with no tile-part POC the main-header POC governs the
        // tile, even when there are no other tile overrides.
        let main_c = cod(2, 2, 2, 0x00, WaveletTransform::Reversible5x3, &[]);
        let main_q = qcd(QuantizationStyle::None, 2, &[0x40, 0x41, 0x42]);
        let (params, coding, quant) = main_defaults(2, &main_c, &main_q);
        let roi = vec![0u32; 2];
        let main_poc = crate::Poc {
            progressions: vec![poc_entry(ProgressionOrder::Rpcl)],
        };
        let resolved =
            resolve_tile_coding(2, &[], Some(&main_poc), &params, &coding, &quant, &roi, &[])
                .expect("resolve");
        assert_eq!(resolved.poc_volumes.len(), 1);
        assert_eq!(resolved.poc_volumes[0].order, ProgressionOrder::Rpcl);
    }

    #[test]
    fn duplicate_tile_poc_is_rejected() {
        // §A.6.6: at most one POC per header.
        let main_c = cod(2, 2, 2, 0x00, WaveletTransform::Reversible5x3, &[]);
        let main_q = qcd(QuantizationStyle::None, 2, &[0x40, 0x41, 0x42]);
        let (params, coding, quant) = main_defaults(2, &main_c, &main_q);
        let roi = vec![0u32; 2];
        let markers = vec![
            crate::TilePartMarker::Poc(crate::Poc {
                progressions: vec![poc_entry(ProgressionOrder::Lrcp)],
            }),
            crate::TilePartMarker::Poc(crate::Poc {
                progressions: vec![poc_entry(ProgressionOrder::Rlcp)],
            }),
        ];
        assert!(
            resolve_tile_coding(2, &[], None, &params, &coding, &quant, &roi, &markers).is_err()
        );
    }
}
