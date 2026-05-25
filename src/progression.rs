//! Progression-order packet iteration — T.800 §B.12.
//!
//! Once a tile's geometry has been derived (`geometry::derive_tile_geometry`
//! → `derive_resolution_levels` → `derive_precinct_partition` →
//! `derive_precinct_code_blocks`) and its tier-2 packet headers can be
//! read (`packet::decode_packet_header`), the remaining piece needed to
//! glue them together is the **order** in which the codestream emits
//! packets across a tile. T.800 §B.12 defines five such orders signalled
//! by the `Ppoc` byte of the `COD` (and overridden by any `POC` marker):
//!
//! | Order | §       | Loop order (outermost → innermost)            |
//! | ----- | ------- | --------------------------------------------- |
//! | LRCP  | §B.12.1.1 | layer → resolution → component → precinct   |
//! | RLCP  | §B.12.1.2 | resolution → layer → component → precinct   |
//! | RPCL  | §B.12.1.3 | resolution → position (y, x) → component → layer |
//! | PCRL  | §B.12.1.4 | position (y, x) → component → resolution → layer |
//! | CPRL  | §B.12.1.5 | component → position (y, x) → resolution → layer |
//!
//! This module implements **LRCP** and **RLCP** — the two progression
//! orders that share the precinct-by-raster-index loop body and differ
//! only in the relative order of the outer two loops. LRCP is the
//! default order (signalled by `Ppoc = 0x00` per T.800 Table A.16) and
//! the most common in practice; RLCP swaps `r` and `l` so the codestream
//! is organised resolution-first (useful when low-resolution previews of
//! every layer are wanted before any layer of any higher resolution).
//! The remaining three orders (RPCL / PCRL / CPRL) replace the per-
//! precinct raster index with the position-iteration machinery of
//! §B.12.1.3 / Equation B-20 (the precinct-step over `(x, y)` walked
//! under the reference-grid divisibility conditions) and land in later
//! rounds.
//!
//! ## LRCP loop body
//!
//! Per T.800 §B.12.1.1:
//!
//! ```text
//! for each l = 0..L                       // layers (from COD or POC)
//!   for each r = 0..=Nmax                 // resolution level (Nmax = max(NL_i))
//!     for each i = 0..Csiz                // component index
//!       for each k = 0..numprecincts(r, i)  // precinct index, raster order
//!         emit packet (component i, resolution r, layer l, precinct k).
//! ```
//!
//! Two corner cases are baked into the loop body:
//!
//! 1. **Components with fewer decomposition levels are skipped at higher
//!    resolutions.** Per §B.12 NOTE: "in this case [different NL per
//!    component], the resolution level that corresponds to the NLLL
//!    sub-band is the first resolution level (r = 0) for all components.
//!    The indices are synchronized from that point on." For a component
//!    `i` with `NL_i < r`, no packet exists at `(l, r, i, *)` and the
//!    inner `k`-loop is skipped entirely.
//!
//! 2. **Empty precincts still produce packets.** §B.6 / §B.9: a precinct
//!    whose code-block partition is empty (every sub-band's grid is `0 ×
//!    0`) is still represented by a packet (whose header carries the
//!    "zero length" bit; the body is empty). The precinct count
//!    `numprecincts(r, i)` therefore includes empty precincts.
//!
//! ## Inputs / output
//!
//! Computing `numprecincts(r, i)` itself lives upstream in
//! [`crate::geometry::derive_precinct_partition`] (and the per-component
//! [`crate::geometry::derive_resolution_levels`] +
//! [`crate::geometry::precinct_exponents_at`] machinery that feeds it).
//! The progression-order driver therefore takes the *result* — a typed
//! [`ComponentProgressionInfo`] per component giving the component's NL
//! and its `numprecincts(r)` for `r = 0..=NL` — and emits the typed
//! [`PacketDescriptor`] sequence.
//!
//! ## Clean-room provenance
//!
//! Built solely against `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf`
//! §B.12 (specifically §B.12.1.1 + §B.12.1.2 — the two four-nested `for`
//! loops differing only by the outer two indices being swapped — and
//! the `Nmax = max_i(NL_i)` definition; the §B.12 NOTE on synchronising
//! the resolution-level index across components with different NL;
//! §B.6 / §B.9 on empty precincts still producing packets). No external
//! library source — OpenJPEG, OpenJPH, Kakadu, Grok, FFmpeg, libavcodec,
//! jpeg2000-rs, etc. — was consulted, quoted, paraphrased, or used as a
//! cross-check oracle. No WebSearch / WebFetch was used for any reason.

use crate::Error;

// ---------------------------------------------------------------------------
// Input description — per-component decomposition + precinct counts.
// ---------------------------------------------------------------------------

/// Per-component input to the §B.12 progression-order driver.
///
/// The driver needs, for each component `i`, two facts:
///
/// 1. `num_decomposition_levels` — `NL_i` from the component's `COD` or
///    `COC` marker (T.800 Table A.15: `0..=32`). Determines the highest
///    resolution level `r` at which the component contributes any packet.
/// 2. `precincts_per_resolution` — `numprecincts(r, i)` for `r = 0..=NL_i`.
///    Indexed by `r`. Each entry is the count returned by
///    [`crate::geometry::derive_precinct_partition`] on the component's
///    `ResolutionLevel`. Empty precincts are still counted per §B.9
///    (their packet bodies are empty but their packet headers must be
///    written).
///
/// The vector must contain exactly `num_decomposition_levels + 1` entries
/// in `r = 0..=NL_i` order. A mismatch is reported via
/// [`Error::InvalidPacketHeader`] from the driver functions — the driver
/// can't recover by guessing what the caller meant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentProgressionInfo {
    /// `NL_i` from `COD` / `COC` — number of wavelet decomposition levels
    /// for this component. The component contributes packets at
    /// resolution levels `r = 0..=num_decomposition_levels`.
    pub num_decomposition_levels: u8,
    /// `numprecincts(r, i)` for `r = 0..=num_decomposition_levels`,
    /// indexed by `r`. Length must equal
    /// `num_decomposition_levels + 1`.
    pub precincts_per_resolution: Vec<u32>,
}

impl ComponentProgressionInfo {
    /// The highest resolution level the component contributes at — i.e.
    /// `num_decomposition_levels` (the component has resolution levels
    /// `r = 0..=NL_i`).
    pub fn max_resolution(&self) -> u8 {
        self.num_decomposition_levels
    }

    /// `numprecincts(r, i)` for this component at resolution `r`.
    ///
    /// Returns `0` for `r > num_decomposition_levels` (the component has
    /// no packets above its top resolution level — the §B.12 NOTE rule).
    pub fn precincts_at(&self, r: u8) -> u32 {
        if r > self.num_decomposition_levels {
            0
        } else {
            self.precincts_per_resolution
                .get(r as usize)
                .copied()
                .unwrap_or(0)
        }
    }

    /// Validates the per-component invariants:
    ///
    /// * `precincts_per_resolution.len() == num_decomposition_levels + 1`
    ///
    /// Returns [`Error::InvalidPacketHeader`] on mismatch.
    pub fn validate(&self) -> Result<(), Error> {
        if self.precincts_per_resolution.len() != self.num_decomposition_levels as usize + 1 {
            return Err(Error::InvalidPacketHeader);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Output — one descriptor per packet in codestream order.
// ---------------------------------------------------------------------------

/// One packet's `(layer, resolution, component, precinct)` coordinates.
///
/// `layer` is the 0-based layer index (T.800 §B.12.1.1 `l`). `resolution`
/// is the 0-based resolution level (`r`). `component` is the 0-based
/// component index (`i`). `precinct` is the 0-based raster precinct index
/// within the (resolution, component) layout (`k`) — same numbering as
/// [`crate::geometry::derive_precinct_code_blocks`]'s `precinct_index`
/// parameter and [`crate::geometry::PrecinctPartition`]'s
/// `num_precincts()` upper bound.
///
/// The descriptor is the **structural** output of the progression order:
/// the actual packet bytes (header + body) are read with
/// [`crate::packet::decode_packet_header`] driving a per-precinct
/// [`crate::packet::PrecinctState`] keyed by `(component, resolution,
/// precinct)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PacketDescriptor {
    /// Layer index `l` (T.800 §B.12.1.1).
    pub layer: u16,
    /// Resolution-level index `r`.
    pub resolution: u8,
    /// Component index `i`.
    pub component: u16,
    /// Raster precinct index `k` within `(resolution, component)`.
    pub precinct: u32,
}

// ---------------------------------------------------------------------------
// LRCP iterator — T.800 §B.12.1.1.
// ---------------------------------------------------------------------------

/// Enumerate one tile's packets in **layer-resolution level-component-
/// position** (LRCP) progression order per T.800 §B.12.1.1.
///
/// `layers` is `L` — the number of quality layers signalled by the `COD`
/// marker (T.800 §A.6.1 / Table A.14). `components` carries one
/// [`ComponentProgressionInfo`] per component, in `Csiz`-declaration order.
///
/// The returned vector lists every packet `(l, r, i, k)` with
/// `l ∈ 0..L`, `r ∈ 0..=Nmax`, `i ∈ 0..components.len()`,
/// `k ∈ 0..numprecincts(r, i)`, skipping any `(l, r, i, *)` for which
/// `r > NL_i` (the §B.12 NOTE rule). `Nmax = max_i(NL_i)`.
///
/// # Loop order (verbatim per §B.12.1.1)
///
/// ```text
/// for each l = 0..L
///   for each r = 0..=Nmax
///     for each i = 0..Csiz
///       for each k = 0..numprecincts(r, i)
/// ```
///
/// # Errors
///
/// * [`Error::InvalidPacketHeader`] if any
///   [`ComponentProgressionInfo::validate`] check fails (per-component
///   `precincts_per_resolution` length mismatch).
/// * [`Error::InvalidComponentCount`] if `components.is_empty()`
///   (T.800 Table A.9 / §A.5: `Csiz` is constrained to `1..=16384`, so a
///   zero-component tile is malformed by construction; we surface that
///   here as a defensive check).
///
/// # Empty-corner behaviour
///
/// * `layers == 0` — returns an empty `Vec` (no packets in any progression
///   order; §B.12.2 lets the `POC` start/end pair define a sub-range, but
///   `L = 0` from the COD itself is degenerate).
/// * A component with `NL_i = 0` and `precincts_per_resolution = [0]`
///   (e.g. an empty tile-component) contributes zero packets — its
///   inner `k`-loop runs `0..0` and emits nothing.
pub fn lrcp_packet_order(
    layers: u16,
    components: &[ComponentProgressionInfo],
) -> Result<Vec<PacketDescriptor>, Error> {
    if components.is_empty() {
        return Err(Error::InvalidComponentCount);
    }
    for ci in components {
        ci.validate()?;
    }
    if layers == 0 {
        return Ok(Vec::new());
    }

    // Nmax = max NL_i across all components. The r-loop runs 0..=Nmax;
    // components with NL_i < r contribute nothing at that r.
    let n_max = components
        .iter()
        .map(|c| c.num_decomposition_levels)
        .max()
        .unwrap_or(0);

    // Conservative upper bound for the output capacity to avoid Vec
    // resize churn on big tiles. Saturating arithmetic so a hostile
    // input doesn't overflow on the multiplications.
    let cap = estimate_packet_count(layers, n_max, components);
    let mut out = Vec::with_capacity(cap);

    for l in 0..layers {
        for r in 0..=n_max {
            for (i, ci) in components.iter().enumerate() {
                if r > ci.num_decomposition_levels {
                    continue;
                }
                let n_pre = ci.precincts_at(r);
                for k in 0..n_pre {
                    out.push(PacketDescriptor {
                        layer: l,
                        resolution: r,
                        component: i as u16,
                        precinct: k,
                    });
                }
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// RLCP iterator — T.800 §B.12.1.2.
// ---------------------------------------------------------------------------

/// Enumerate one tile's packets in **resolution level-layer-component-
/// position** (RLCP) progression order per T.800 §B.12.1.2.
///
/// `layers` is `L` — the number of quality layers signalled by the `COD`
/// marker (T.800 §A.6.1 / Table A.14). `components` carries one
/// [`ComponentProgressionInfo`] per component, in `Csiz`-declaration order.
///
/// The returned vector lists every packet `(r, l, i, k)` with
/// `r ∈ 0..=Nmax`, `l ∈ 0..L`, `i ∈ 0..components.len()`,
/// `k ∈ 0..numprecincts(r, i)`, skipping any `(r, l, i, *)` for which
/// `r > NL_i` (the §B.12 NOTE rule). `Nmax = max_i(NL_i)`.
///
/// # Loop order (verbatim per §B.12.1.2)
///
/// ```text
/// for each r = 0..=Nmax
///   for each l = 0..L
///     for each i = 0..Csiz
///       for each k = 0..numprecincts(r, i)
/// ```
///
/// RLCP differs from [`lrcp_packet_order`] only in the position of the
/// `r` and `l` loops: LRCP iterates layer-first (every layer's worth of
/// resolution levels appears together), RLCP iterates resolution-first
/// (every resolution level's worth of layers appears together). The
/// inner two loops (`i`, `k`), the `Nmax` definition, the §B.12 NOTE
/// rule on components with `NL_i < r`, and the §B.6 / §B.9 rule on
/// empty precincts still producing packets are all identical.
///
/// # Errors
///
/// * [`Error::InvalidPacketHeader`] if any
///   [`ComponentProgressionInfo::validate`] check fails (per-component
///   `precincts_per_resolution` length mismatch).
/// * [`Error::InvalidComponentCount`] if `components.is_empty()`
///   (T.800 Table A.9 / §A.5: `Csiz` is constrained to `1..=16384`).
///
/// # Empty-corner behaviour
///
/// * `layers == 0` — returns an empty `Vec` (the inner `l`-loop runs
///   `0..0`, so every `r` contributes nothing).
/// * A component with `NL_i = 0` and `precincts_per_resolution = [0]`
///   contributes zero packets — its innermost `k`-loop runs `0..0`.
pub fn rlcp_packet_order(
    layers: u16,
    components: &[ComponentProgressionInfo],
) -> Result<Vec<PacketDescriptor>, Error> {
    if components.is_empty() {
        return Err(Error::InvalidComponentCount);
    }
    for ci in components {
        ci.validate()?;
    }
    if layers == 0 {
        return Ok(Vec::new());
    }

    // Nmax = max NL_i across all components. The r-loop runs 0..=Nmax;
    // components with NL_i < r contribute nothing at that r.
    let n_max = components
        .iter()
        .map(|c| c.num_decomposition_levels)
        .max()
        .unwrap_or(0);

    // Total packet count is invariant under r↔l swap, so the LRCP
    // estimator gives the correct capacity hint here too.
    let cap = estimate_packet_count(layers, n_max, components);
    let mut out = Vec::with_capacity(cap);

    for r in 0..=n_max {
        for l in 0..layers {
            for (i, ci) in components.iter().enumerate() {
                if r > ci.num_decomposition_levels {
                    continue;
                }
                let n_pre = ci.precincts_at(r);
                for k in 0..n_pre {
                    out.push(PacketDescriptor {
                        layer: l,
                        resolution: r,
                        component: i as u16,
                        precinct: k,
                    });
                }
            }
        }
    }
    Ok(out)
}

/// Saturating estimate of total packet count for the LRCP / RLCP loop
/// (the two orders enumerate the same set of `(l, r, i, k)` tuples and
/// therefore have identical totals — only the order in which they're
/// emitted differs):
/// `L * sum_r sum_i numprecincts(r, i)` where the `r`-sum runs over
/// `0..=NL_i`. Used only as a `Vec::with_capacity` hint.
fn estimate_packet_count(layers: u16, n_max: u8, components: &[ComponentProgressionInfo]) -> usize {
    let mut per_layer: usize = 0;
    for r in 0..=n_max {
        for ci in components {
            if r > ci.num_decomposition_levels {
                continue;
            }
            per_layer = per_layer.saturating_add(ci.precincts_at(r) as usize);
        }
    }
    per_layer.saturating_mul(layers as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Single-layer, single-component, NL = 0 (1 resolution level), one
    /// precinct. LRCP emits exactly that one packet.
    #[test]
    fn lrcp_minimal_one_packet() {
        let comps = vec![ComponentProgressionInfo {
            num_decomposition_levels: 0,
            precincts_per_resolution: vec![1],
        }];
        let out = lrcp_packet_order(1, &comps).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0],
            PacketDescriptor {
                layer: 0,
                resolution: 0,
                component: 0,
                precinct: 0
            }
        );
    }

    /// One layer, one component, NL = 2 (3 resolution levels), one
    /// precinct each → 3 packets in r = 0, 1, 2.
    #[test]
    fn lrcp_resolutions_in_order() {
        let comps = vec![ComponentProgressionInfo {
            num_decomposition_levels: 2,
            precincts_per_resolution: vec![1, 1, 1],
        }];
        let out = lrcp_packet_order(1, &comps).unwrap();
        let resolutions: Vec<u8> = out.iter().map(|p| p.resolution).collect();
        assert_eq!(resolutions, vec![0, 1, 2]);
    }

    /// Two layers, one component, one resolution, one precinct → 2
    /// packets in l = 0, then l = 1.
    #[test]
    fn lrcp_layers_outermost() {
        let comps = vec![ComponentProgressionInfo {
            num_decomposition_levels: 0,
            precincts_per_resolution: vec![1],
        }];
        let out = lrcp_packet_order(2, &comps).unwrap();
        let layers: Vec<u16> = out.iter().map(|p| p.layer).collect();
        assert_eq!(layers, vec![0, 1]);
    }

    /// One layer, three components in raster order at the same resolution
    /// level: components interleave at r = 0.
    #[test]
    fn lrcp_components_interleave_within_resolution() {
        let comps = vec![
            ComponentProgressionInfo {
                num_decomposition_levels: 0,
                precincts_per_resolution: vec![1],
            },
            ComponentProgressionInfo {
                num_decomposition_levels: 0,
                precincts_per_resolution: vec![1],
            },
            ComponentProgressionInfo {
                num_decomposition_levels: 0,
                precincts_per_resolution: vec![1],
            },
        ];
        let out = lrcp_packet_order(1, &comps).unwrap();
        let comps_out: Vec<u16> = out.iter().map(|p| p.component).collect();
        assert_eq!(comps_out, vec![0, 1, 2]);
    }

    /// Within one (l, r, i), precincts are emitted in raster order
    /// (`k = 0, 1, 2, ...`).
    #[test]
    fn lrcp_precincts_in_raster_order_within_component() {
        let comps = vec![ComponentProgressionInfo {
            num_decomposition_levels: 0,
            precincts_per_resolution: vec![4],
        }];
        let out = lrcp_packet_order(1, &comps).unwrap();
        let ks: Vec<u32> = out.iter().map(|p| p.precinct).collect();
        assert_eq!(ks, vec![0, 1, 2, 3]);
    }

    /// Full nested order — the spec's verbatim §B.12.1.1 loop body.
    /// 2 layers × 2 resolutions × 2 components × 2 precincts = 16
    /// packets, listed in the exact order layer→resolution→component→
    /// precinct.
    #[test]
    fn lrcp_full_nested_order_2_2_2_2() {
        let comps = vec![
            ComponentProgressionInfo {
                num_decomposition_levels: 1,
                precincts_per_resolution: vec![2, 2],
            },
            ComponentProgressionInfo {
                num_decomposition_levels: 1,
                precincts_per_resolution: vec![2, 2],
            },
        ];
        let out = lrcp_packet_order(2, &comps).unwrap();
        assert_eq!(out.len(), 16);

        // Hand-build the expected sequence per the §B.12.1.1 nested loop.
        let mut expected: Vec<PacketDescriptor> = Vec::with_capacity(16);
        for l in 0u16..2 {
            for r in 0u8..=1 {
                for i in 0u16..2 {
                    for k in 0u32..2 {
                        expected.push(PacketDescriptor {
                            layer: l,
                            resolution: r,
                            component: i,
                            precinct: k,
                        });
                    }
                }
            }
        }
        assert_eq!(out, expected);
    }

    /// §B.12 NOTE: components with different NL synchronise at r = 0,
    /// and a component with NL_i < r contributes nothing at that r.
    /// Two components with NL = 6 and NL = 2 respectively (so 7 and 3
    /// resolution levels). Nmax = 6. At r = 3..=6 only component 0
    /// contributes; at r = 0..=2 both interleave.
    #[test]
    fn lrcp_components_with_different_nl_synchronise_at_r0() {
        let comps = vec![
            ComponentProgressionInfo {
                num_decomposition_levels: 6,
                precincts_per_resolution: vec![1; 7],
            },
            ComponentProgressionInfo {
                num_decomposition_levels: 2,
                precincts_per_resolution: vec![1; 3],
            },
        ];
        let out = lrcp_packet_order(1, &comps).unwrap();
        // Expected count per layer: 3 r-levels × 2 components + 4 r-levels
        // × 1 component (only c=0) = 6 + 4 = 10.
        assert_eq!(out.len(), 10);

        // Verify r = 0..=2 has both components interleaved, r = 3..=6
        // has only component 0.
        let pairs: Vec<(u8, u16)> = out.iter().map(|p| (p.resolution, p.component)).collect();
        assert_eq!(
            pairs,
            vec![
                (0, 0),
                (0, 1),
                (1, 0),
                (1, 1),
                (2, 0),
                (2, 1),
                (3, 0),
                (4, 0),
                (5, 0),
                (6, 0),
            ]
        );
    }

    /// Empty-precinct corner: a resolution level with `numprecincts = 0`
    /// emits zero packets at that level, but does not skip the rest of
    /// the r-loop. (§B.6: a degenerate resolution level with no precincts
    /// just contributes nothing.)
    #[test]
    fn lrcp_zero_precinct_resolution_emits_nothing() {
        let comps = vec![ComponentProgressionInfo {
            num_decomposition_levels: 2,
            precincts_per_resolution: vec![1, 0, 1],
        }];
        let out = lrcp_packet_order(1, &comps).unwrap();
        let rs: Vec<u8> = out.iter().map(|p| p.resolution).collect();
        // r = 0 and r = 2 each contribute one packet, r = 1 contributes
        // none.
        assert_eq!(rs, vec![0, 2]);
    }

    /// `layers = 0` → empty progression (no packets).
    #[test]
    fn lrcp_zero_layers_emits_no_packets() {
        let comps = vec![ComponentProgressionInfo {
            num_decomposition_levels: 0,
            precincts_per_resolution: vec![1],
        }];
        let out = lrcp_packet_order(0, &comps).unwrap();
        assert!(out.is_empty());
    }

    /// Empty components vector → `Error::InvalidComponentCount`.
    /// (Defensive: T.800 Table A.9 constrains `Csiz` to `1..=16384`.)
    #[test]
    fn lrcp_empty_components_rejected() {
        let out = lrcp_packet_order(1, &[]);
        assert!(matches!(out, Err(Error::InvalidComponentCount)));
    }

    /// `precincts_per_resolution.len() != NL + 1` → `Error::
    /// InvalidPacketHeader`. The vector must carry exactly one entry per
    /// resolution level `r = 0..=NL_i`.
    #[test]
    fn lrcp_per_component_length_mismatch_rejected() {
        let comps = vec![ComponentProgressionInfo {
            num_decomposition_levels: 2,
            // length should be 3, not 2.
            precincts_per_resolution: vec![1, 1],
        }];
        let out = lrcp_packet_order(1, &comps);
        assert!(matches!(out, Err(Error::InvalidPacketHeader)));
    }

    /// `precincts_at(r)` past the top resolution level returns 0, not a
    /// panic from out-of-range Vec indexing.
    #[test]
    fn precincts_at_past_top_resolution_returns_zero() {
        let ci = ComponentProgressionInfo {
            num_decomposition_levels: 1,
            precincts_per_resolution: vec![1, 2],
        };
        assert_eq!(ci.precincts_at(0), 1);
        assert_eq!(ci.precincts_at(1), 2);
        assert_eq!(ci.precincts_at(2), 0);
        assert_eq!(ci.precincts_at(255), 0);
    }

    /// `max_resolution()` echoes `num_decomposition_levels`.
    #[test]
    fn max_resolution_matches_nl() {
        let ci = ComponentProgressionInfo {
            num_decomposition_levels: 5,
            precincts_per_resolution: vec![1; 6],
        };
        assert_eq!(ci.max_resolution(), 5);
    }

    /// Single-component LRCP: every emitted descriptor has `component = 0`
    /// and the sequence sorts lexicographically by `(layer, resolution,
    /// precinct)`.
    #[test]
    fn lrcp_single_component_orders_l_then_r_then_k() {
        let comps = vec![ComponentProgressionInfo {
            num_decomposition_levels: 1,
            precincts_per_resolution: vec![2, 3],
        }];
        // L = 2 → 2 layers × (2 + 3) precincts = 10 packets per layer
        // doubled = 10 packets per layer? Actually it's r=0 (2 precincts)
        // + r=1 (3 precincts) = 5 packets per layer; × 2 layers = 10.
        let out = lrcp_packet_order(2, &comps).unwrap();
        assert_eq!(out.len(), 10);

        // Per-layer slice expectations.
        let layer0: Vec<(u8, u32)> = out
            .iter()
            .filter(|p| p.layer == 0)
            .map(|p| (p.resolution, p.precinct))
            .collect();
        assert_eq!(
            layer0,
            vec![(0, 0), (0, 1), (1, 0), (1, 1), (1, 2)],
            "layer 0 emits r=0 precincts before r=1 precincts"
        );
        let layer1: Vec<(u8, u32)> = out
            .iter()
            .filter(|p| p.layer == 1)
            .map(|p| (p.resolution, p.precinct))
            .collect();
        assert_eq!(layer1, layer0, "layer 1 repeats the same (r, k) shape");
        // All component fields are zero (single-component tile).
        assert!(out.iter().all(|p| p.component == 0));
    }

    /// Capacity hint is a saturating upper bound — verify it equals the
    /// actual output length on a non-degenerate input (no skipped
    /// components, no zero-precinct levels).
    #[test]
    fn capacity_estimate_matches_output_when_no_skips() {
        let comps = vec![
            ComponentProgressionInfo {
                num_decomposition_levels: 1,
                precincts_per_resolution: vec![2, 2],
            },
            ComponentProgressionInfo {
                num_decomposition_levels: 1,
                precincts_per_resolution: vec![2, 2],
            },
        ];
        let n_max = 1;
        let est = estimate_packet_count(3, n_max, &comps);
        let out = lrcp_packet_order(3, &comps).unwrap();
        assert_eq!(est, out.len());
    }

    /// §B.12 NOTE worked example transcribed verbatim: two components
    /// with 7 and 3 resolution levels. Within a single layer, total
    /// packet count = 3 levels × 2 components + 4 levels × 1 component
    /// = 10 (one precinct per (r, i) for the verification).
    #[test]
    fn lrcp_b12_note_worked_example() {
        let comps = vec![
            // Component 0: 7 resolution levels (NL = 6).
            ComponentProgressionInfo {
                num_decomposition_levels: 6,
                precincts_per_resolution: vec![1; 7],
            },
            // Component 1: 3 resolution levels (NL = 2).
            ComponentProgressionInfo {
                num_decomposition_levels: 2,
                precincts_per_resolution: vec![1; 3],
            },
        ];
        let out = lrcp_packet_order(1, &comps).unwrap();
        // r = 0, 1, 2: both components → 6 packets.
        // r = 3, 4, 5, 6: only component 0 → 4 packets.
        // Total: 10.
        assert_eq!(out.len(), 10);
        let r3_plus: Vec<&PacketDescriptor> = out.iter().filter(|p| p.resolution >= 3).collect();
        assert!(
            r3_plus.iter().all(|p| p.component == 0),
            "components past their NL contribute nothing at higher r"
        );
        assert_eq!(r3_plus.len(), 4);
    }

    // -----------------------------------------------------------------------
    // RLCP — T.800 §B.12.1.2. r↔l swap vs. LRCP; everything else identical.
    // -----------------------------------------------------------------------

    /// Single-layer, single-component, NL = 0 (1 resolution level), one
    /// precinct. RLCP emits exactly that one packet (identical to LRCP
    /// since the swap is degenerate when L = 1 and Nmax = 0).
    #[test]
    fn rlcp_minimal_one_packet() {
        let comps = vec![ComponentProgressionInfo {
            num_decomposition_levels: 0,
            precincts_per_resolution: vec![1],
        }];
        let out = rlcp_packet_order(1, &comps).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0],
            PacketDescriptor {
                layer: 0,
                resolution: 0,
                component: 0,
                precinct: 0
            }
        );
    }

    /// Resolution is the outermost index per §B.12.1.2. One component,
    /// NL = 2 (3 resolution levels), 2 layers, one precinct per level →
    /// emit (r=0,l=0), (r=0,l=1), (r=1,l=0), (r=1,l=1), (r=2,l=0),
    /// (r=2,l=1).
    #[test]
    fn rlcp_resolution_outermost_layers_inner() {
        let comps = vec![ComponentProgressionInfo {
            num_decomposition_levels: 2,
            precincts_per_resolution: vec![1, 1, 1],
        }];
        let out = rlcp_packet_order(2, &comps).unwrap();
        assert_eq!(out.len(), 6);
        let rl: Vec<(u8, u16)> = out.iter().map(|p| (p.resolution, p.layer)).collect();
        assert_eq!(rl, vec![(0, 0), (0, 1), (1, 0), (1, 1), (2, 0), (2, 1)]);
    }

    /// Two layers, one component, one resolution, one precinct → still
    /// 2 packets, but in l = 0, 1 (within the single r = 0). Same as
    /// LRCP here because Nmax = 0 collapses the swap.
    #[test]
    fn rlcp_layers_inner_within_single_resolution() {
        let comps = vec![ComponentProgressionInfo {
            num_decomposition_levels: 0,
            precincts_per_resolution: vec![1],
        }];
        let out = rlcp_packet_order(2, &comps).unwrap();
        let layers: Vec<u16> = out.iter().map(|p| p.layer).collect();
        assert_eq!(layers, vec![0, 1]);
    }

    /// One layer, three components in raster order at the same resolution
    /// level: components interleave at r = 0 inside the (single) layer
    /// loop.
    #[test]
    fn rlcp_components_interleave_within_layer() {
        let comps = vec![
            ComponentProgressionInfo {
                num_decomposition_levels: 0,
                precincts_per_resolution: vec![1],
            },
            ComponentProgressionInfo {
                num_decomposition_levels: 0,
                precincts_per_resolution: vec![1],
            },
            ComponentProgressionInfo {
                num_decomposition_levels: 0,
                precincts_per_resolution: vec![1],
            },
        ];
        let out = rlcp_packet_order(1, &comps).unwrap();
        let comps_out: Vec<u16> = out.iter().map(|p| p.component).collect();
        assert_eq!(comps_out, vec![0, 1, 2]);
    }

    /// Within one (r, l, i), precincts are emitted in raster order
    /// (`k = 0, 1, 2, ...`) — same innermost loop as LRCP.
    #[test]
    fn rlcp_precincts_in_raster_order_within_component() {
        let comps = vec![ComponentProgressionInfo {
            num_decomposition_levels: 0,
            precincts_per_resolution: vec![4],
        }];
        let out = rlcp_packet_order(1, &comps).unwrap();
        let ks: Vec<u32> = out.iter().map(|p| p.precinct).collect();
        assert_eq!(ks, vec![0, 1, 2, 3]);
    }

    /// Full nested order — the spec's verbatim §B.12.1.2 loop body.
    /// 2 layers × 2 resolutions × 2 components × 2 precincts = 16
    /// packets, listed resolution → layer → component → precinct.
    #[test]
    fn rlcp_full_nested_order_2_2_2_2() {
        let comps = vec![
            ComponentProgressionInfo {
                num_decomposition_levels: 1,
                precincts_per_resolution: vec![2, 2],
            },
            ComponentProgressionInfo {
                num_decomposition_levels: 1,
                precincts_per_resolution: vec![2, 2],
            },
        ];
        let out = rlcp_packet_order(2, &comps).unwrap();
        assert_eq!(out.len(), 16);

        // Hand-build the expected sequence per the §B.12.1.2 nested loop.
        let mut expected: Vec<PacketDescriptor> = Vec::with_capacity(16);
        for r in 0u8..=1 {
            for l in 0u16..2 {
                for i in 0u16..2 {
                    for k in 0u32..2 {
                        expected.push(PacketDescriptor {
                            layer: l,
                            resolution: r,
                            component: i,
                            precinct: k,
                        });
                    }
                }
            }
        }
        assert_eq!(out, expected);
    }

    /// §B.12 NOTE: components with different NL synchronise at r = 0
    /// in RLCP exactly as in LRCP. Two components with NL = 6 and
    /// NL = 2 respectively. At r = 3..=6 only component 0 contributes.
    /// Verified across one layer.
    #[test]
    fn rlcp_components_with_different_nl_synchronise_at_r0() {
        let comps = vec![
            ComponentProgressionInfo {
                num_decomposition_levels: 6,
                precincts_per_resolution: vec![1; 7],
            },
            ComponentProgressionInfo {
                num_decomposition_levels: 2,
                precincts_per_resolution: vec![1; 3],
            },
        ];
        let out = rlcp_packet_order(1, &comps).unwrap();
        // Total per single layer: 3 r-levels × 2 components + 4 r-levels
        // × 1 component = 10.
        assert_eq!(out.len(), 10);
        let pairs: Vec<(u8, u16)> = out.iter().map(|p| (p.resolution, p.component)).collect();
        assert_eq!(
            pairs,
            vec![
                (0, 0),
                (0, 1),
                (1, 0),
                (1, 1),
                (2, 0),
                (2, 1),
                (3, 0),
                (4, 0),
                (5, 0),
                (6, 0),
            ]
        );
    }

    /// Empty-precinct corner: a resolution level with `numprecincts = 0`
    /// emits zero packets at that level but does not skip the rest of
    /// the r-loop. (§B.6 / §B.9 — identical to the LRCP corner case.)
    #[test]
    fn rlcp_zero_precinct_resolution_emits_nothing() {
        let comps = vec![ComponentProgressionInfo {
            num_decomposition_levels: 2,
            precincts_per_resolution: vec![1, 0, 1],
        }];
        let out = rlcp_packet_order(1, &comps).unwrap();
        let rs: Vec<u8> = out.iter().map(|p| p.resolution).collect();
        assert_eq!(rs, vec![0, 2]);
    }

    /// `layers = 0` → empty progression (no packets). Inner l-loop runs
    /// `0..0` for every r, so nothing is emitted.
    #[test]
    fn rlcp_zero_layers_emits_no_packets() {
        let comps = vec![ComponentProgressionInfo {
            num_decomposition_levels: 0,
            precincts_per_resolution: vec![1],
        }];
        let out = rlcp_packet_order(0, &comps).unwrap();
        assert!(out.is_empty());
    }

    /// Empty components vector → `Error::InvalidComponentCount`. Matches
    /// the LRCP defensive check.
    #[test]
    fn rlcp_empty_components_rejected() {
        let out = rlcp_packet_order(1, &[]);
        assert!(matches!(out, Err(Error::InvalidComponentCount)));
    }

    /// `precincts_per_resolution.len() != NL + 1` → `Error::
    /// InvalidPacketHeader`. Same per-component validation as LRCP.
    #[test]
    fn rlcp_per_component_length_mismatch_rejected() {
        let comps = vec![ComponentProgressionInfo {
            num_decomposition_levels: 2,
            precincts_per_resolution: vec![1, 1],
        }];
        let out = rlcp_packet_order(1, &comps);
        assert!(matches!(out, Err(Error::InvalidPacketHeader)));
    }

    /// LRCP and RLCP enumerate the **same** set of packet descriptors —
    /// the swap only reorders them. Verify by sorting both outputs and
    /// comparing.
    #[test]
    fn lrcp_and_rlcp_emit_same_multiset() {
        let comps = vec![
            ComponentProgressionInfo {
                num_decomposition_levels: 2,
                precincts_per_resolution: vec![3, 2, 1],
            },
            ComponentProgressionInfo {
                num_decomposition_levels: 1,
                precincts_per_resolution: vec![2, 1],
            },
        ];
        let layers = 3;
        let mut lrcp = lrcp_packet_order(layers, &comps).unwrap();
        let mut rlcp = rlcp_packet_order(layers, &comps).unwrap();
        assert_eq!(lrcp.len(), rlcp.len());
        // Sort by (l, r, c, k) and assert equality.
        let key = |p: &PacketDescriptor| (p.layer, p.resolution, p.component, p.precinct);
        lrcp.sort_by_key(key);
        rlcp.sort_by_key(key);
        assert_eq!(lrcp, rlcp);
    }

    /// LRCP and RLCP differ as soon as both L > 1 and Nmax > 0 — the
    /// outermost descriptor is layer-first vs. resolution-first.
    /// Confirm the prefix actually differs on a small (L=2, NL=1) input.
    #[test]
    fn lrcp_and_rlcp_differ_at_outer_loop() {
        let comps = vec![ComponentProgressionInfo {
            num_decomposition_levels: 1,
            precincts_per_resolution: vec![1, 1],
        }];
        let lrcp = lrcp_packet_order(2, &comps).unwrap();
        let rlcp = rlcp_packet_order(2, &comps).unwrap();
        // Both have 4 packets total.
        assert_eq!(lrcp.len(), 4);
        assert_eq!(rlcp.len(), 4);
        // LRCP: (l=0,r=0), (l=0,r=1), (l=1,r=0), (l=1,r=1).
        let lrcp_lr: Vec<(u16, u8)> = lrcp.iter().map(|p| (p.layer, p.resolution)).collect();
        assert_eq!(lrcp_lr, vec![(0, 0), (0, 1), (1, 0), (1, 1)]);
        // RLCP: (r=0,l=0), (r=0,l=1), (r=1,l=0), (r=1,l=1).
        let rlcp_rl: Vec<(u8, u16)> = rlcp.iter().map(|p| (p.resolution, p.layer)).collect();
        assert_eq!(rlcp_rl, vec![(0, 0), (0, 1), (1, 0), (1, 1)]);
    }

    /// Single-component RLCP: every emitted descriptor has `component =
    /// 0` and the sequence sorts lexicographically by `(resolution,
    /// layer, precinct)`.
    #[test]
    fn rlcp_single_component_orders_r_then_l_then_k() {
        let comps = vec![ComponentProgressionInfo {
            num_decomposition_levels: 1,
            precincts_per_resolution: vec![2, 3],
        }];
        // r = 0 contributes 2 precincts × 2 layers = 4 packets;
        // r = 1 contributes 3 precincts × 2 layers = 6 packets; total 10.
        let out = rlcp_packet_order(2, &comps).unwrap();
        assert_eq!(out.len(), 10);

        // Per-resolution slice expectations.
        let r0: Vec<(u16, u32)> = out
            .iter()
            .filter(|p| p.resolution == 0)
            .map(|p| (p.layer, p.precinct))
            .collect();
        assert_eq!(
            r0,
            vec![(0, 0), (0, 1), (1, 0), (1, 1)],
            "r=0 emits all layers (and all their precincts) before moving to r=1"
        );
        let r1: Vec<(u16, u32)> = out
            .iter()
            .filter(|p| p.resolution == 1)
            .map(|p| (p.layer, p.precinct))
            .collect();
        assert_eq!(
            r1,
            vec![(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2)],
            "r=1 emits all layers' worth of its 3 precincts"
        );
        assert!(out.iter().all(|p| p.component == 0));
    }

    /// Capacity hint is shared between LRCP and RLCP (same total). Verify
    /// it equals the actual RLCP output length on a non-degenerate input.
    #[test]
    fn rlcp_capacity_estimate_matches_output_when_no_skips() {
        let comps = vec![
            ComponentProgressionInfo {
                num_decomposition_levels: 1,
                precincts_per_resolution: vec![2, 2],
            },
            ComponentProgressionInfo {
                num_decomposition_levels: 1,
                precincts_per_resolution: vec![2, 2],
            },
        ];
        let n_max = 1;
        let est = estimate_packet_count(3, n_max, &comps);
        let out = rlcp_packet_order(3, &comps).unwrap();
        assert_eq!(est, out.len());
    }

    /// §B.12 NOTE worked example again, but verified through RLCP. With
    /// L = 2, total packets = 10 (per-layer) × 2 = 20. Sweep the output
    /// and confirm: at r ∈ 3..=6, every packet has component 0; at r ∈
    /// 0..=2, components 0 and 1 alternate within each layer.
    #[test]
    fn rlcp_b12_note_worked_example_two_layers() {
        let comps = vec![
            ComponentProgressionInfo {
                num_decomposition_levels: 6,
                precincts_per_resolution: vec![1; 7],
            },
            ComponentProgressionInfo {
                num_decomposition_levels: 2,
                precincts_per_resolution: vec![1; 3],
            },
        ];
        let out = rlcp_packet_order(2, &comps).unwrap();
        // 10 per layer × 2 layers = 20.
        assert_eq!(out.len(), 20);
        // For each r in 0..=2 the r-block contains 2 layers × 2 components
        // = 4 packets (each precinct count is 1). For each r in 3..=6 the
        // r-block contains 2 layers × 1 component = 2 packets.
        let r0_block: Vec<(u16, u16)> = out
            .iter()
            .filter(|p| p.resolution == 0)
            .map(|p| (p.layer, p.component))
            .collect();
        assert_eq!(r0_block, vec![(0, 0), (0, 1), (1, 0), (1, 1)]);
        let r5_block: Vec<(u16, u16)> = out
            .iter()
            .filter(|p| p.resolution == 5)
            .map(|p| (p.layer, p.component))
            .collect();
        assert_eq!(r5_block, vec![(0, 0), (1, 0)]);
        // Every packet at r >= 3 has component 0.
        assert!(out
            .iter()
            .filter(|p| p.resolution >= 3)
            .all(|p| p.component == 0));
    }
}
