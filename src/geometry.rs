//! Image-area + tile-grid + tile-component coordinate derivation from
//! the SIZ marker segment.
//!
//! All derivations follow T.800 (ITU-T Rec. T.800 (06/2019) | ISO/IEC
//! 15444-1) §B.2 / §B.3 / §B.5 — Equations B-1, B-2, B-3, B-4, B-5,
//! B-6, B-7, B-8, B-9, B-10, B-11, B-12, B-13.
//!
//! The entry point is [`derive_tile_geometry`]: given a parsed [`Siz`]
//! and a tile-grid index `t` (the `Isot` value from a `SOT` marker),
//! it returns a [`TileGeometry`] carrying both the tile's bounding box
//! on the reference grid (`(tx0, ty0)` / `(tx1, ty1)`) and a
//! [`TileComponentGeometry`] for each component giving the per-component
//! sample bounds `(tcx0, tcy0)` / `(tcx1, tcy1)` and dimensions.
//!
//! Image-area (whole-image) per-component dimensions per Equation B-1 +
//! B-2 are exposed via [`image_area`], and the tile-grid extent
//! `(numXtiles, numYtiles)` per Equation B-5 via [`tile_grid_extent`].
//!
//! # Clean-room provenance
//!
//! Built solely against
//! `docs/image/jpeg2000/T-REC-T.800-201906-S.pdf` — §B.2 (Image area
//! definition), §B.3 (Image area division into tiles and
//! tile-components, Equations B-3 / B-4 / B-5 / B-6 / B-7 / B-8 / B-9 /
//! B-10 / B-11), §B.4 (worked example with two-component 1432×954
//! reference grid the test corpus also targets), §B.5 (Equation B-12 /
//! B-13 for the per-component sample mapping). No external library
//! source was consulted.

use crate::{Error, Siz};

/// Image-area corners on the **component** domain (Equation B-1) for
/// one component.
///
/// `x0` / `y0` are the upper-left sample's coordinates; `x1 - 1` /
/// `y1 - 1` are the lower-right sample's coordinates. Per-component
/// width / height per Equation B-2 are `width = x1 - x0`, `height =
/// y1 - y0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageAreaComponent {
    /// `x0` per Equation B-1: `ceil(XOsiz / XRsizc)`.
    pub x0: u32,
    /// `y0` per Equation B-1: `ceil(YOsiz / YRsizc)`.
    pub y0: u32,
    /// `x1` per Equation B-1: `ceil(Xsiz / XRsizc)`.
    pub x1: u32,
    /// `y1` per Equation B-1: `ceil(Ysiz / YRsizc)`.
    pub y1: u32,
}

impl ImageAreaComponent {
    /// Per-component width per Equation B-2: `x1 - x0`.
    pub fn width(&self) -> u32 {
        self.x1.saturating_sub(self.x0)
    }

    /// Per-component height per Equation B-2: `y1 - y0`.
    pub fn height(&self) -> u32 {
        self.y1.saturating_sub(self.y0)
    }
}

/// Per-component tile-component coordinates on the **component**
/// domain (Equation B-12 / B-13).
///
/// `tcx0` / `tcy0` are the upper-left sample of the tile-component;
/// `tcx1 - 1` / `tcy1 - 1` are the lower-right sample. Dimensions per
/// Equation B-13 are `width = tcx1 - tcx0`, `height = tcy1 - tcy0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileComponentGeometry {
    /// `tcx0` per Equation B-12: `ceil(tx0 / XRsizi)`.
    pub tcx0: u32,
    /// `tcy0` per Equation B-12: `ceil(ty0 / YRsizi)`.
    pub tcy0: u32,
    /// `tcx1` per Equation B-12: `ceil(tx1 / XRsizi)`.
    pub tcx1: u32,
    /// `tcy1` per Equation B-12: `ceil(ty1 / YRsizi)`.
    pub tcy1: u32,
}

impl TileComponentGeometry {
    /// Tile-component width per Equation B-13: `tcx1 - tcx0`.
    pub fn width(&self) -> u32 {
        self.tcx1.saturating_sub(self.tcx0)
    }

    /// Tile-component height per Equation B-13: `tcy1 - tcy0`.
    pub fn height(&self) -> u32 {
        self.tcy1.saturating_sub(self.tcy0)
    }
}

/// Geometry of a single tile on the reference grid plus one
/// [`TileComponentGeometry`] per component.
///
/// Returned by [`derive_tile_geometry`]. Reference-grid bounds follow
/// Equations B-7 / B-8 / B-9 / B-10; per-component bounds follow
/// Equation B-12.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileGeometry {
    /// Tile index `t` from the `SOT` marker's `Isot` field.
    pub tile_index: u32,
    /// Horizontal tile-grid coordinate `p` per Equation B-6:
    /// `t mod numXtiles`.
    pub p: u32,
    /// Vertical tile-grid coordinate `q` per Equation B-6:
    /// `floor(t / numXtiles)`.
    pub q: u32,
    /// `tx0(p, q)` per Equation B-7: `max(XTOsiz + p * XTsiz, XOsiz)`.
    pub tx0: u32,
    /// `ty0(p, q)` per Equation B-8: `max(YTOsiz + q * YTsiz, YOsiz)`.
    pub ty0: u32,
    /// `tx1(p, q)` per Equation B-9:
    /// `min(XTOsiz + (p + 1) * XTsiz, Xsiz)`.
    pub tx1: u32,
    /// `ty1(p, q)` per Equation B-10:
    /// `min(YTOsiz + (q + 1) * YTsiz, Ysiz)`.
    pub ty1: u32,
    /// One [`TileComponentGeometry`] per component, in the same order
    /// as [`Siz::components`].
    pub components: Vec<TileComponentGeometry>,
}

impl TileGeometry {
    /// Tile width on the reference grid per Equation B-11:
    /// `tx1 - tx0`.
    pub fn width(&self) -> u32 {
        self.tx1.saturating_sub(self.tx0)
    }

    /// Tile height on the reference grid per Equation B-11:
    /// `ty1 - ty0`.
    pub fn height(&self) -> u32 {
        self.ty1.saturating_sub(self.ty0)
    }
}

// ---------------------------------------------------------------------------
// SIZ-level checks + arithmetic helpers.
// ---------------------------------------------------------------------------

/// `ceil(a / b)` for `u32` values, returning [`Error::InvalidMarkerLength`]
/// on `b == 0` (which is itself a SIZ-parser-level invariant — `XRsizi`
/// / `YRsizi` are constrained to `1..=255` per T.800 Table A.9).
#[inline]
fn ceil_div_u32(a: u32, b: u32) -> Result<u32, Error> {
    if b == 0 {
        // The SIZ parser already rejects XR=0/YR=0 (Table A.9), so this
        // is defence-in-depth for SIZ inputs constructed via other
        // means.
        return Err(Error::InvalidMarkerLength);
    }
    // ceil(a/b) = floor((a + b - 1) / b). Use checked add to keep u32
    // overflow surfaced rather than wrap-around.
    let sum = a.checked_add(b - 1).ok_or(Error::InvalidMarkerLength)?;
    Ok(sum / b)
}

/// Validate the SIZ parameters against T.800 Equations B-3 / B-4 and
/// the requirement that the reference grid is non-empty (Xsiz > XOsiz,
/// Ysiz > YOsiz).
///
/// The SIZ marker parser ([`crate::parse_j2k_header`]) does not enforce
/// these inter-field invariants in round 1 (it only checks the per-field
/// ranges in Table A.9); this function is the round-6 entry point that
/// checks them when the caller asks for geometry derivation. Returning
/// [`Error::InvalidMarkerLength`] for any failure keeps the error
/// surface consistent with the existing parser.
pub fn validate_siz(siz: &Siz) -> Result<(), Error> {
    // §B.2 implicitly requires Xsiz > XOsiz, Ysiz > YOsiz for a
    // non-empty image area.
    if siz.x_size <= siz.x_offset || siz.y_size <= siz.y_offset {
        return Err(Error::InvalidMarkerLength);
    }
    // Equation B-3: 0 <= XTOsiz <= XOsiz, 0 <= YTOsiz <= YOsiz.
    if siz.tile_x_offset > siz.x_offset || siz.tile_y_offset > siz.y_offset {
        return Err(Error::InvalidMarkerLength);
    }
    // Equation B-4: XTsiz + XTOsiz > XOsiz, YTsiz + YTOsiz > YOsiz.
    let xt_plus = siz
        .tile_width
        .checked_add(siz.tile_x_offset)
        .ok_or(Error::InvalidMarkerLength)?;
    if xt_plus <= siz.x_offset {
        return Err(Error::InvalidMarkerLength);
    }
    let yt_plus = siz
        .tile_height
        .checked_add(siz.tile_y_offset)
        .ok_or(Error::InvalidMarkerLength)?;
    if yt_plus <= siz.y_offset {
        return Err(Error::InvalidMarkerLength);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Image-area derivation (Equations B-1 / B-2).
// ---------------------------------------------------------------------------

/// Compute the per-component image area on the component domain for
/// every component in `siz`.
///
/// Per T.800 §B.2 / Equation B-1, each component's bounding sample
/// rectangle on its own (sub-sampled) domain is given by:
///
/// * `x0_c = ceil(XOsiz / XRsizc)`
/// * `y0_c = ceil(YOsiz / YRsizc)`
/// * `x1_c = ceil(Xsiz  / XRsizc)`
/// * `y1_c = ceil(Ysiz  / YRsizc)`
///
/// and the component's overall width / height follow per Equation B-2
/// as `x1 - x0` / `y1 - y0`.
///
/// SIZ must satisfy [`validate_siz`]'s invariants; the function calls
/// it internally and propagates [`Error::InvalidMarkerLength`] on
/// failure.
pub fn image_area(siz: &Siz) -> Result<Vec<ImageAreaComponent>, Error> {
    validate_siz(siz)?;
    let mut out = Vec::with_capacity(siz.components.len());
    for c in &siz.components {
        let xr = c.h_separation as u32;
        let yr = c.v_separation as u32;
        out.push(ImageAreaComponent {
            x0: ceil_div_u32(siz.x_offset, xr)?,
            y0: ceil_div_u32(siz.y_offset, yr)?,
            x1: ceil_div_u32(siz.x_size, xr)?,
            y1: ceil_div_u32(siz.y_size, yr)?,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tile-grid extent (Equation B-5).
// ---------------------------------------------------------------------------

/// Number of tiles on the tile grid in (x, y), per T.800 Equation B-5:
///
/// * `numXtiles = ceil((Xsiz - XTOsiz) / XTsiz)`
/// * `numYtiles = ceil((Ysiz - YTOsiz) / YTsiz)`
///
/// SIZ must satisfy [`validate_siz`]'s invariants; the function calls
/// it internally and propagates the error on failure.
pub fn tile_grid_extent(siz: &Siz) -> Result<(u32, u32), Error> {
    validate_siz(siz)?;
    // XTsiz and YTsiz are constrained to `1..=2^32 - 1` per Table A.9
    // (zero would have failed `validate_siz`'s B-4 check above since
    // XTOsiz <= XOsiz < Xsiz).
    let num_x = ceil_div_u32(siz.x_size - siz.tile_x_offset, siz.tile_width)?;
    let num_y = ceil_div_u32(siz.y_size - siz.tile_y_offset, siz.tile_height)?;
    Ok((num_x, num_y))
}

// ---------------------------------------------------------------------------
// Per-tile geometry derivation (Equations B-6 / B-7..B-10 / B-12 / B-13).
// ---------------------------------------------------------------------------

/// Derive the geometry for tile `t` (the `Isot` value from a `SOT`
/// marker) on the reference grid and on each component domain.
///
/// `t` must be strictly less than `numXtiles * numYtiles` per T.800
/// §A.4.2 (`Isot` is the raster-order tile index). The function returns
/// [`Error::InvalidTilePartIndex`] if `t` is out of range.
///
/// Reference-grid bounds follow Equations B-7 / B-8 / B-9 / B-10:
///
/// * `tx0(p, q) = max(XTOsiz + p * XTsiz, XOsiz)`
/// * `ty0(p, q) = max(YTOsiz + q * YTsiz, YOsiz)`
/// * `tx1(p, q) = min(XTOsiz + (p + 1) * XTsiz, Xsiz)`
/// * `ty1(p, q) = min(YTOsiz + (q + 1) * YTsiz, Ysiz)`
///
/// Per-component bounds follow Equation B-12:
///
/// * `tcx0 = ceil(tx0 / XRsizi)`
/// * `tcy0 = ceil(ty0 / YRsizi)`
/// * `tcx1 = ceil(tx1 / XRsizi)`
/// * `tcy1 = ceil(ty1 / YRsizi)`
///
/// for each component index `i`.
pub fn derive_tile_geometry(siz: &Siz, t: u32) -> Result<TileGeometry, Error> {
    validate_siz(siz)?;
    let (num_x, num_y) = tile_grid_extent(siz)?;
    let total = (num_x as u64) * (num_y as u64);
    if (t as u64) >= total {
        return Err(Error::InvalidTilePartIndex);
    }
    // Equation B-6: p = t mod numXtiles, q = floor(t / numXtiles).
    // numXtiles is non-zero because validate_siz ensures Xsiz > XOsiz
    // and XTOsiz <= XOsiz so (Xsiz - XTOsiz) > 0 and ceil_div by XTsiz
    // produces at least 1.
    let p = t % num_x;
    let q = t / num_x;

    // Equations B-7..B-10. Use u64 internally for `XTOsiz + (p+1)*XTsiz`
    // to avoid overflow on the extreme corner (XTsiz close to u32::MAX),
    // then clip against Xsiz/Ysiz which are u32 by spec.
    let tx0_raw = (siz.tile_x_offset as u64) + (p as u64) * (siz.tile_width as u64);
    let ty0_raw = (siz.tile_y_offset as u64) + (q as u64) * (siz.tile_height as u64);
    let tx1_raw = (siz.tile_x_offset as u64) + ((p as u64) + 1) * (siz.tile_width as u64);
    let ty1_raw = (siz.tile_y_offset as u64) + ((q as u64) + 1) * (siz.tile_height as u64);

    let tx0 = tx0_raw.max(siz.x_offset as u64).min(u32::MAX as u64) as u32;
    let ty0 = ty0_raw.max(siz.y_offset as u64).min(u32::MAX as u64) as u32;
    let tx1 = tx1_raw.min(siz.x_size as u64) as u32;
    let ty1 = ty1_raw.min(siz.y_size as u64) as u32;

    // A degenerate tile would have tx1 <= tx0 — Equations B-3..B-5
    // guarantee the first row/column of tiles contains at least one
    // reference grid point, but interior-corner tiles past the right or
    // bottom edge can be zero-area. The spec permits that (Equation
    // B-11 width can be < XTsiz at the edge). We surface it as a
    // non-error so callers see the empty bounds.
    if tx1 < tx0 || ty1 < ty0 {
        return Err(Error::InvalidMarkerLength);
    }

    // Equation B-12 per component.
    let mut components = Vec::with_capacity(siz.components.len());
    for c in &siz.components {
        let xr = c.h_separation as u32;
        let yr = c.v_separation as u32;
        components.push(TileComponentGeometry {
            tcx0: ceil_div_u32(tx0, xr)?,
            tcy0: ceil_div_u32(ty0, yr)?,
            tcx1: ceil_div_u32(tx1, xr)?,
            tcy1: ceil_div_u32(ty1, yr)?,
        });
    }

    Ok(TileGeometry {
        tile_index: t,
        p,
        q,
        tx0,
        ty0,
        tx1,
        ty1,
        components,
    })
}

// ---------------------------------------------------------------------------
// Tests — synthetic SIZ values driving the §B.4 worked example plus
// edge-case coverage for the per-component ceiling math.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SizComponent;

    /// SIZ helper — the §B.4 worked example: 1432×954 reference grid,
    /// XOsiz = 152, YOsiz = 234, XTsiz = 396, YTsiz = 297, XTOsiz =
    /// YTOsiz = 0, two components with sub-sampling (1, 1) and (2, 2).
    fn siz_b4_worked_example() -> Siz {
        Siz {
            rsiz: 0,
            x_size: 1432,
            y_size: 954,
            x_offset: 152,
            y_offset: 234,
            tile_width: 396,
            tile_height: 297,
            tile_x_offset: 0,
            tile_y_offset: 0,
            components: vec![
                SizComponent {
                    precision_bits: 8,
                    is_signed: false,
                    h_separation: 1,
                    v_separation: 1,
                },
                SizComponent {
                    precision_bits: 8,
                    is_signed: false,
                    h_separation: 2,
                    v_separation: 2,
                },
            ],
        }
    }

    #[test]
    fn image_area_matches_b4_component_0() {
        // §B.4: component 0 upper-left = (152, 234); lower-right =
        // (1431, 953); actual size 1280×720.
        let siz = siz_b4_worked_example();
        let area = image_area(&siz).expect("image_area");
        assert_eq!(area.len(), 2);
        assert_eq!(area[0].x0, 152);
        assert_eq!(area[0].y0, 234);
        assert_eq!(area[0].x1, 1432);
        assert_eq!(area[0].y1, 954);
        assert_eq!(area[0].width(), 1280);
        assert_eq!(area[0].height(), 720);
    }

    #[test]
    fn image_area_matches_b4_component_1() {
        // §B.4: component 1 upper-left = (76, 117); lower-right =
        // (715, 476); actual size 640×360.
        let siz = siz_b4_worked_example();
        let area = image_area(&siz).expect("image_area");
        assert_eq!(area[1].x0, 76);
        assert_eq!(area[1].y0, 117);
        assert_eq!(area[1].x1, 716);
        assert_eq!(area[1].y1, 477);
        assert_eq!(area[1].width(), 640);
        assert_eq!(area[1].height(), 360);
    }

    #[test]
    fn tile_grid_extent_matches_b4() {
        // §B.4: numXtiles = 4, numYtiles = 4 → 16 tiles total.
        let siz = siz_b4_worked_example();
        let (nx, ny) = tile_grid_extent(&siz).expect("tile_grid_extent");
        assert_eq!(nx, 4);
        assert_eq!(ny, 4);
    }

    #[test]
    fn derive_tile_geometry_reference_grid_matches_b4_corners() {
        // §B.4 quotes tx0(0:3,*) = {152, 396, 792, 1188} and tx1(0:3,*)
        // = {396, 792, 1188, 1432}. Likewise ty0(*,0:3) = {234, 297,
        // 594, 891}, ty1(*,0:3) = {297, 594, 891, 954}.
        let siz = siz_b4_worked_example();
        let expected_tx0 = [152u32, 396, 792, 1188];
        let expected_tx1 = [396u32, 792, 1188, 1432];
        let expected_ty0 = [234u32, 297, 594, 891];
        let expected_ty1 = [297u32, 594, 891, 954];
        for p in 0u32..4 {
            for q in 0u32..4 {
                let t = q * 4 + p;
                let g = derive_tile_geometry(&siz, t).expect("derive");
                assert_eq!(g.p, p, "p for t={t}");
                assert_eq!(g.q, q, "q for t={t}");
                assert_eq!(g.tx0, expected_tx0[p as usize], "tx0 for t={t}");
                assert_eq!(g.tx1, expected_tx1[p as usize], "tx1 for t={t}");
                assert_eq!(g.ty0, expected_ty0[q as usize], "ty0 for t={t}");
                assert_eq!(g.ty1, expected_ty1[q as usize], "ty1 for t={t}");
            }
        }
    }

    #[test]
    fn derive_tile_geometry_interior_component_0_matches_b4() {
        // §B.4: tiles (1,1), (1,2), (2,1), (2,2) on component 0 are all
        // (XRsiz0, YRsiz0) = (1, 1), so per-component dims equal
        // reference-grid dims. Tile (1,1) is at (396, 297)..(792, 594),
        // so component-0 dims are 396×297.
        let siz = siz_b4_worked_example();
        for &(p, q) in &[(1u32, 1u32), (1, 2), (2, 1), (2, 2)] {
            let t = q * 4 + p;
            let g = derive_tile_geometry(&siz, t).expect("derive");
            let c0 = g.components[0];
            assert_eq!(c0.width(), 396, "p={p} q={q} comp0 width");
            assert_eq!(c0.height(), 297, "p={p} q={q} comp0 height");
        }
    }

    #[test]
    fn derive_tile_geometry_interior_component_1_matches_b4() {
        // §B.4: on component 1 (sub-sampling 2, 2):
        // tiles (1,1) and (2,1) are 198×148; tiles (1,2) and (2,2) are
        // 198×149. The asymmetry arises because:
        //   tile (1,1) ref-grid is (396,297)..(792,594) →
        //     ceil(792/2)-ceil(396/2) = 396-198 = 198,
        //     ceil(594/2)-ceil(297/2) = 297-149 = 148.
        //   tile (1,2) ref-grid is (396,594)..(792,891) →
        //     ceil(891/2)-ceil(594/2) = 446-297 = 149.
        let siz = siz_b4_worked_example();
        let cases = [
            ((1u32, 1u32), (198u32, 148u32)),
            ((2, 1), (198, 148)),
            ((1, 2), (198, 149)),
            ((2, 2), (198, 149)),
        ];
        for ((p, q), (w, h)) in cases {
            let t = q * 4 + p;
            let g = derive_tile_geometry(&siz, t).expect("derive");
            let c1 = g.components[1];
            assert_eq!(c1.width(), w, "p={p} q={q} comp1 width");
            assert_eq!(c1.height(), h, "p={p} q={q} comp1 height");
        }
    }

    #[test]
    fn derive_tile_geometry_first_tile_clamped_to_image_offset() {
        // Tile 0 on the §B.4 worked example: tx0 = max(0+0*396, 152) =
        // 152, tx1 = min(0+1*396, 1432) = 396. Same for y → ty0=234,
        // ty1=297. Equation B-7..B-10 corner case where the image
        // offset clips the tile.
        let siz = siz_b4_worked_example();
        let g = derive_tile_geometry(&siz, 0).expect("derive");
        assert_eq!(g.tx0, 152);
        assert_eq!(g.ty0, 234);
        assert_eq!(g.tx1, 396);
        assert_eq!(g.ty1, 297);
        // Width = 396 - 152 = 244 on the reference grid.
        assert_eq!(g.width(), 244);
        assert_eq!(g.height(), 63);
    }

    #[test]
    fn derive_tile_geometry_last_tile_clamped_to_image_extent() {
        // Tile 15 = (p, q) = (3, 3): tx0 = max(0+3*396, 152) = 1188,
        // tx1 = min(0+4*396, 1432) = 1432. Width = 244.
        let siz = siz_b4_worked_example();
        let g = derive_tile_geometry(&siz, 15).expect("derive");
        assert_eq!(g.p, 3);
        assert_eq!(g.q, 3);
        assert_eq!(g.tx0, 1188);
        assert_eq!(g.tx1, 1432);
        assert_eq!(g.ty0, 891);
        assert_eq!(g.ty1, 954);
    }

    #[test]
    fn derive_tile_geometry_rejects_out_of_range_index() {
        // numXtiles * numYtiles = 16 → tile index 16 is out of range.
        let siz = siz_b4_worked_example();
        let err = derive_tile_geometry(&siz, 16).unwrap_err();
        assert_eq!(err, Error::InvalidTilePartIndex);
    }

    #[test]
    fn single_tile_single_component_grid() {
        // The synth_minimal_header SIZ: 1×1 grid, single component, no
        // sub-sampling. numXtiles = numYtiles = 1.
        let siz = Siz {
            rsiz: 0,
            x_size: 1,
            y_size: 1,
            x_offset: 0,
            y_offset: 0,
            tile_width: 1,
            tile_height: 1,
            tile_x_offset: 0,
            tile_y_offset: 0,
            components: vec![SizComponent {
                precision_bits: 8,
                is_signed: false,
                h_separation: 1,
                v_separation: 1,
            }],
        };
        assert_eq!(tile_grid_extent(&siz).unwrap(), (1, 1));
        let g = derive_tile_geometry(&siz, 0).unwrap();
        assert_eq!(g.tx0, 0);
        assert_eq!(g.tx1, 1);
        assert_eq!(g.ty0, 0);
        assert_eq!(g.ty1, 1);
        let c0 = g.components[0];
        assert_eq!(c0.width(), 1);
        assert_eq!(c0.height(), 1);
    }

    #[test]
    fn validate_siz_rejects_xt_offset_greater_than_x_offset() {
        // Equation B-3: 0 <= XTOsiz <= XOsiz.
        let mut siz = siz_b4_worked_example();
        siz.tile_x_offset = siz.x_offset + 1;
        let err = validate_siz(&siz).unwrap_err();
        assert_eq!(err, Error::InvalidMarkerLength);
    }

    #[test]
    fn validate_siz_rejects_xt_plus_offset_le_x_offset() {
        // Equation B-4: XTsiz + XTOsiz > XOsiz. Setting XTsiz = 1 and
        // XTOsiz = 0 with XOsiz = 152 gives XTsiz + XTOsiz = 1 < 152,
        // violating B-4.
        let mut siz = siz_b4_worked_example();
        siz.tile_width = 1;
        siz.tile_x_offset = 0;
        let err = validate_siz(&siz).unwrap_err();
        assert_eq!(err, Error::InvalidMarkerLength);
    }

    #[test]
    fn validate_siz_rejects_empty_image_area() {
        // §B.2: image area must be non-empty (Xsiz > XOsiz).
        let mut siz = siz_b4_worked_example();
        siz.x_offset = siz.x_size;
        let err = validate_siz(&siz).unwrap_err();
        assert_eq!(err, Error::InvalidMarkerLength);
    }

    #[test]
    fn tile_grid_extent_handles_non_zero_tile_offset() {
        // §B.3: when XTOsiz != 0, numXtiles = ceil((Xsiz - XTOsiz) / XTsiz).
        // Construct: Xsiz = 1000, XTOsiz = 50, XTsiz = 100 →
        // numXtiles = ceil(950 / 100) = 10. Pair with XOsiz = 100 so
        // B-3 holds.
        let siz = Siz {
            rsiz: 0,
            x_size: 1000,
            y_size: 1000,
            x_offset: 100,
            y_offset: 100,
            tile_width: 100,
            tile_height: 100,
            tile_x_offset: 50,
            tile_y_offset: 50,
            components: vec![SizComponent {
                precision_bits: 8,
                is_signed: false,
                h_separation: 1,
                v_separation: 1,
            }],
        };
        assert_eq!(tile_grid_extent(&siz).unwrap(), (10, 10));
    }

    #[test]
    fn derive_tile_geometry_three_to_one_subsampling_floor_corner() {
        // Construct: 100×100 grid, no offset, single 100×100 tile, two
        // components with XRsiz = 1 and XRsiz = 3 respectively (a YUV
        // 4:1:1-like sub-sampling). Component-1's per-tile width should
        // be ceil(100/3) - ceil(0/3) = 34. The §B.5 worked example
        // illustrates this kind of asymmetric ceiling-divide for the
        // sub-sampled component.
        let siz = Siz {
            rsiz: 0,
            x_size: 100,
            y_size: 100,
            x_offset: 0,
            y_offset: 0,
            tile_width: 100,
            tile_height: 100,
            tile_x_offset: 0,
            tile_y_offset: 0,
            components: vec![
                SizComponent {
                    precision_bits: 8,
                    is_signed: false,
                    h_separation: 1,
                    v_separation: 1,
                },
                SizComponent {
                    precision_bits: 8,
                    is_signed: false,
                    h_separation: 3,
                    v_separation: 1,
                },
            ],
        };
        let g = derive_tile_geometry(&siz, 0).unwrap();
        assert_eq!(g.components[0].width(), 100);
        // ceil(100/3) - ceil(0/3) = 34 - 0 = 34.
        assert_eq!(g.components[1].width(), 34);
        assert_eq!(g.components[1].height(), 100);
    }

    #[test]
    fn image_area_three_to_one_subsampling_floor_corner() {
        // For the same shape as above, image_area should yield
        // component-1 width = ceil(100/3) - ceil(0/3) = 34.
        let siz = Siz {
            rsiz: 0,
            x_size: 100,
            y_size: 100,
            x_offset: 0,
            y_offset: 0,
            tile_width: 100,
            tile_height: 100,
            tile_x_offset: 0,
            tile_y_offset: 0,
            components: vec![
                SizComponent {
                    precision_bits: 8,
                    is_signed: false,
                    h_separation: 1,
                    v_separation: 1,
                },
                SizComponent {
                    precision_bits: 8,
                    is_signed: false,
                    h_separation: 3,
                    v_separation: 1,
                },
            ],
        };
        let area = image_area(&siz).expect("image_area");
        assert_eq!(area[1].x0, 0);
        assert_eq!(area[1].x1, 34);
        assert_eq!(area[1].width(), 34);
    }
}
