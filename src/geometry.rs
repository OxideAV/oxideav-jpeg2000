//! Image-area + tile-grid + tile-component coordinate derivation from
//! the SIZ marker segment, plus per-resolution-level + per-sub-band
//! geometry from the COD marker's `NL` (number of decomposition levels),
//! plus the precinct partition (§B.6) and code-block partition (§B.7)
//! of each resolution level.
//!
//! All derivations follow T.800 (ITU-T Rec. T.800 (06/2019) | ISO/IEC
//! 15444-1) §B.2 / §B.3 / §B.5 / §B.6 / §B.7 — Equations B-1, B-2, B-3,
//! B-4, B-5, B-6, B-7, B-8, B-9, B-10, B-11, B-12, B-13, B-14, B-15,
//! B-16, B-17, B-18 plus Table B.1 (sub-band orientation displacements
//! `(xob, yob)`), Table A.18 (code-block exponents), and Table A.21
//! (precinct exponents).
//!
//! The tile-level entry point is [`derive_tile_geometry`]: given a
//! parsed [`Siz`] and a tile-grid index `t` (the `Isot` value from a
//! `SOT` marker), it returns a [`TileGeometry`] carrying both the
//! tile's bounding box on the reference grid (`(tx0, ty0)` /
//! `(tx1, ty1)`) and a [`TileComponentGeometry`] for each component
//! giving the per-component sample bounds `(tcx0, tcy0)` /
//! `(tcx1, tcy1)` and dimensions.
//!
//! The resolution-level + sub-band entry point is
//! [`derive_resolution_levels`]: given one [`TileComponentGeometry`]
//! and `NL` (number of decomposition levels from the `COD` or `COC`
//! marker), it returns a `Vec<ResolutionLevel>` of length `NL + 1`,
//! one per resolution level `r = 0..=NL`. Each [`ResolutionLevel`]
//! carries its own `(trx0, try0, trx1, try1)` per Equation B-14 plus a
//! `Vec<SubBand>` with one entry per sub-band: just the `LL` band at
//! `r = 0` (the "NLLL" band, §B.5 lead-in), and `{ HL, LH, HH }` at
//! every `r ≥ 1`. Each [`SubBand`] carries `(tbx0, tby0, tbx1, tby1)`
//! per Equation B-15 with the orientation displacements `(xob, yob)`
//! looked up from Table B.1.
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
//! B-13 for the per-component sample mapping; Equation B-14
//! resolution-level corners; Equation B-15 sub-band corners; Table
//! B.1 sub-band orientation displacements `(xob, yob)` for the four
//! sub-bands `LL`, `HL`, `LH`, `HH`); §B.6 (Equation B-16 precinct
//! count from the `PPx` / `PPy` exponents, with Table A.21 nibble
//! layout and the Table A.13 maximum-precinct default `PPx = PPy = 15`);
//! §B.7 (Equation B-17 / Equation B-18 effective code-block exponents
//! clamped to the precinct, with Table A.18 `xcb = value + 2`). No
//! external library source was consulted.

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
// Per-resolution-level + per-sub-band geometry (T.800 §B.5 — Equation
// B-14, Equation B-15, Table B.1).
// ---------------------------------------------------------------------------

/// Sub-band orientation per T.800 §B.5 / Table B.1.
///
/// The four orientations partition the wavelet-decomposed tile-component
/// at each decomposition level. At resolution level `r = 0` only the
/// `LL` band is present (the "nLL" lead-in to §B.5); at every
/// resolution level `r ≥ 1` only the three high-pass bands `HL`, `LH`,
/// `HH` are present (the `LL` portion at `r ≥ 1` is implicit in the
/// next-lower-`r` resolution level and is not stored as a sub-band of
/// the same resolution level).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubBandOrientation {
    /// `nLL` — both filters low-pass.
    /// Table B.1: `(xob, yob) = (0, 0)`. Present only at `r = 0`.
    LL,
    /// `nHL` — horizontal high-pass, vertical low-pass.
    /// Table B.1: `(xob, yob) = (1, 0)`. Present only at `r ≥ 1`.
    HL,
    /// `nLH` — horizontal low-pass, vertical high-pass.
    /// Table B.1: `(xob, yob) = (0, 1)`. Present only at `r ≥ 1`.
    LH,
    /// `nHH` — both filters high-pass.
    /// Table B.1: `(xob, yob) = (1, 1)`. Present only at `r ≥ 1`.
    HH,
}

impl SubBandOrientation {
    /// Orientation-displacement `xob` per T.800 Table B.1.
    pub fn xob(self) -> u32 {
        match self {
            SubBandOrientation::LL | SubBandOrientation::LH => 0,
            SubBandOrientation::HL | SubBandOrientation::HH => 1,
        }
    }

    /// Orientation-displacement `yob` per T.800 Table B.1.
    pub fn yob(self) -> u32 {
        match self {
            SubBandOrientation::LL | SubBandOrientation::HL => 0,
            SubBandOrientation::LH | SubBandOrientation::HH => 1,
        }
    }
}

/// One sub-band's bounding-sample rectangle on its own sub-band domain
/// per T.800 §B.5 / Equation B-15.
///
/// `tbx0` / `tby0` are the upper-left sample's coordinates; `tbx1 - 1`
/// / `tby1 - 1` are the lower-right sample's coordinates. Width is
/// `tbx1 - tbx0`, height is `tby1 - tby0` (§B.5 closing paragraph).
///
/// `nb` is the decomposition level associated with this sub-band per
/// T.800 §B.5 / Annex F:
///
/// * `LL` at `r = 0`: `nb = NL`.
/// * `HL` / `LH` / `HH` at resolution level `r` (`r ≥ 1`): `nb =
///   NL - r + 1`.
///
/// The relationship `nb = n + 1 = NL - r + 1` for the three high-pass
/// bands at resolution `r` reflects the wavelet pyramid: synthesising
/// `LL_{NL - r}` (the LL band one resolution-level higher than `r - 1`)
/// requires the `HL`, `LH`, `HH` bands from decomposition level
/// `NL - r + 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubBand {
    /// Sub-band orientation (one of `LL`, `HL`, `LH`, `HH`).
    pub orientation: SubBandOrientation,
    /// Decomposition level `nb` associated with this sub-band (`1..=NL`
    /// for high-pass bands; `NL` for the `LL` band at `r = 0`).
    pub nb: u8,
    /// `tbx0` per Equation B-15.
    pub tbx0: u32,
    /// `tby0` per Equation B-15.
    pub tby0: u32,
    /// `tbx1` per Equation B-15.
    pub tbx1: u32,
    /// `tby1` per Equation B-15.
    pub tby1: u32,
}

impl SubBand {
    /// Sub-band width per §B.5: `tbx1 - tbx0`.
    pub fn width(&self) -> u32 {
        self.tbx1.saturating_sub(self.tbx0)
    }

    /// Sub-band height per §B.5: `tby1 - tby0`.
    pub fn height(&self) -> u32 {
        self.tby1.saturating_sub(self.tby0)
    }
}

/// One resolution level's reduced-resolution rectangle on the
/// tile-component domain per T.800 §B.5 / Equation B-14, plus the
/// sub-bands that contribute to it.
///
/// `r = 0` is the lowest resolution (the "NLLL band"); `r = NL` is the
/// full tile-component resolution. The denominator in Equation B-14 is
/// `2^(NL - r)`, so `r = 0` carries the most-reduced rectangle and
/// `r = NL` carries the full tile-component rectangle (equal to
/// `TileComponentGeometry` itself).
///
/// The `sub_bands` vector holds:
///
/// * **one** entry at `r = 0`, the `LL` band, with the same corners
///   as `(trx0, try0, trx1, try1)`. This reflects §B.5's lead-in:
///   "The lowest resolution level, r = 0, is represented by the NLLL
///   band."
/// * **three** entries at every `r ≥ 1`, the `HL`, `LH`, `HH` bands
///   in that order, each with their own `(tbx0, tby0, tbx1, tby1)`
///   computed from Equation B-15 with the orientation displacements
///   from Table B.1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionLevel {
    /// Resolution-level index, `0..=NL`.
    pub r: u8,
    /// Number of decomposition levels signalled by COD/COC for this
    /// component. Stored on every level to keep the struct
    /// self-describing.
    pub n_l: u8,
    /// `trx0` per Equation B-14: `ceil(tcx0 / 2^(NL - r))`.
    pub trx0: u32,
    /// `try0` per Equation B-14: `ceil(tcy0 / 2^(NL - r))`.
    pub try0: u32,
    /// `trx1` per Equation B-14: `ceil(tcx1 / 2^(NL - r))`.
    pub trx1: u32,
    /// `try1` per Equation B-14: `ceil(tcy1 / 2^(NL - r))`.
    pub try1: u32,
    /// One `SubBand` per orientation present at this resolution level:
    /// `[LL]` at `r = 0`, `[HL, LH, HH]` at `r ≥ 1`.
    pub sub_bands: Vec<SubBand>,
}

impl ResolutionLevel {
    /// Reduced-resolution width per §B.5: `trx1 - trx0`.
    pub fn width(&self) -> u32 {
        self.trx1.saturating_sub(self.trx0)
    }

    /// Reduced-resolution height per §B.5: `try1 - try0`.
    pub fn height(&self) -> u32 {
        self.try1.saturating_sub(self.try0)
    }
}

/// Reduced-resolution `trx` value per Equation B-14:
/// `ceil(tc / 2^(NL - r))`.
///
/// `NL - r` is `n` in §B.5's notation. Result is computed as the
/// closed-form `(tc + (1 << n) - 1) >> n` for non-zero `n`, falling
/// back to the identity when `n == 0` (i.e. `r == NL`).
#[inline]
fn ceil_div_pow2(tc: u32, n: u32) -> u32 {
    if n == 0 {
        tc
    } else if n >= 32 {
        // Saturate: 2^32 or more reduces any u32 to zero or one. The
        // tile-component rectangle is bounded by Xsiz/Ysiz which fit in
        // u32, so `tc` is at most `2^32 - 1`; `ceil(tc / 2^n)` is
        // therefore `(tc != 0) as u32` for `n >= 32`. The COD parser
        // bounds NL to `0..=32` (Table A.15), so `n` here is at most
        // `NL` = 32 and we hit this branch only for r = 0, NL = 32.
        if tc == 0 {
            0
        } else {
            1
        }
    } else {
        let step = 1u64 << n;
        let numer = tc as u64 + step - 1;
        // `numer / step` fits in u32 because `numer <= 2^32 - 1 + 2^32 - 1
        // < 2^33` and `step >= 2`.
        (numer / step) as u32
    }
}

/// One sub-band corner (`tbx0`, `tby0`, `tbx1`, or `tby1`) per T.800
/// §B.5 / Equation B-15:
///
/// ```text
/// tbN = ceil((tcN - 2^(nb - 1) * ob) / 2^nb)
/// ```
///
/// where `N ∈ {0, 1}`, `tcN` is the corresponding tile-component
/// corner, and `ob ∈ {0, 1}` is the orientation displacement from
/// Table B.1 (`xob` for x, `yob` for y).
///
/// The subtraction is done in signed `i64` arithmetic to surface the
/// "less than `2^(nb - 1) * ob`" corner: Eq B-15 specifies a ceiling
/// of a value that can be negative when `tcN = 0` and `ob = 1`. With
/// ceiling division on a non-positive numerator, the result is `≤ 0`
/// and clamps to zero — a negative sub-band corner has no meaning on
/// the sub-band domain, and §B.5 implicitly assumes the resulting
/// `tbN` are non-negative for any well-formed tile-component.
#[inline]
fn subband_corner(tc: u32, nb: u32, ob: u32) -> u32 {
    debug_assert!(ob <= 1);
    debug_assert!((1..=32).contains(&nb));
    // Numerator in Eq B-15 is `tc - 2^(nb - 1) * ob`.
    let offset = if ob == 0 {
        0i64
    } else {
        // 2^(nb - 1): nb is in 1..=32 so the shift fits in i64.
        1i64 << (nb - 1)
    };
    let numer = tc as i64 - offset;
    // Denominator: 2^nb. Bounded by 2^32 for nb = 32. Fits in u64.
    let denom = 1u64 << nb;
    if numer >= 0 {
        let pos = numer as u64;
        pos.div_ceil(denom) as u32
    } else {
        // numer < 0. ceil(numer / denom) is a non-positive integer;
        // clamp to zero per the doc comment above.
        0
    }
}

/// Derive the per-resolution-level + per-sub-band geometry for one
/// tile-component, given `NL` decomposition levels from the relevant
/// `COD` / `COC` marker.
///
/// Returns a `Vec<ResolutionLevel>` of length `NL + 1`, indexed by `r`.
/// Each level's reference-grid rectangle follows Equation B-14 and its
/// sub-bands follow Equation B-15 with the orientation displacements
/// `(xob, yob)` from Table B.1:
///
/// * `r = 0` carries a single `SubBand` with orientation `LL`, `nb =
///   NL`, and the same corners as the resolution level itself (§B.5
///   lead-in: "The lowest resolution level, r = 0, is represented by
///   the NLLL band.").
/// * `r ≥ 1` carries three sub-bands with orientations `HL`, `LH`,
///   `HH` in that order, each at `nb = NL - r + 1`. The `LL` band at
///   `r ≥ 1` is the lower resolution level's rectangle and is **not**
///   re-emitted here.
///
/// `n_l` is constrained by `COD` Table A.15 to `0..=32`. `n_l = 0` is
/// the no-transform corner: a single resolution level `r = 0` with one
/// `LL` sub-band identical to the tile-component itself.
pub fn derive_resolution_levels(tc: TileComponentGeometry, n_l: u8) -> Vec<ResolutionLevel> {
    debug_assert!(n_l <= 32, "NL is constrained to 0..=32 per Table A.15");

    let mut levels = Vec::with_capacity((n_l as usize) + 1);
    for r in 0..=n_l {
        let n = (n_l - r) as u32;
        let trx0 = ceil_div_pow2(tc.tcx0, n);
        let try0 = ceil_div_pow2(tc.tcy0, n);
        let trx1 = ceil_div_pow2(tc.tcx1, n);
        let try1 = ceil_div_pow2(tc.tcy1, n);

        let sub_bands = if r == 0 {
            // §B.5 lead-in: r = 0 is the NLLL band. Single LL sub-band
            // at decomposition level nb = NL. Its corners coincide with
            // the resolution-level corners — Eq B-15 with (xob, yob) =
            // (0, 0) reduces to `ceil(tc / 2^nb)`, which is exactly Eq
            // B-14's `ceil(tc / 2^(NL - 0))`.
            vec![SubBand {
                orientation: SubBandOrientation::LL,
                nb: n_l,
                tbx0: trx0,
                tby0: try0,
                tbx1: trx1,
                tby1: try1,
            }]
        } else {
            // r ≥ 1: HL, LH, HH at decomposition level nb = NL - r + 1.
            let nb = (n_l - r + 1) as u32;
            let mut bands = Vec::with_capacity(3);
            for orientation in [
                SubBandOrientation::HL,
                SubBandOrientation::LH,
                SubBandOrientation::HH,
            ] {
                let xob = orientation.xob();
                let yob = orientation.yob();
                bands.push(SubBand {
                    orientation,
                    nb: nb as u8,
                    tbx0: subband_corner(tc.tcx0, nb, xob),
                    tby0: subband_corner(tc.tcy0, nb, yob),
                    tbx1: subband_corner(tc.tcx1, nb, xob),
                    tby1: subband_corner(tc.tcy1, nb, yob),
                });
            }
            bands
        };

        levels.push(ResolutionLevel {
            r,
            n_l,
            trx0,
            try0,
            trx1,
            try1,
            sub_bands,
        });
    }
    levels
}

// ---------------------------------------------------------------------------
// Precinct partitioning (T.800 §B.6 — Equation B-16) and code-block
// partitioning (T.800 §B.7 — Equation B-17 / Equation B-18).
// ---------------------------------------------------------------------------

/// The precinct exponents `(PPx, PPy)` in force at one resolution level,
/// per T.800 §B.6 / Table A.21.
///
/// `PPx` / `PPy` are the base-2 exponents of the precinct width / height
/// (the precinct is `2^PPx` wide by `2^PPy` high on the reduced-
/// resolution tile-component domain). They are signalled per
/// tile-component and per resolution level in the `COD` / `COC` marker
/// (Table A.21: the low nibble of each precinct byte is `PPx`, the high
/// nibble is `PPy`). When no user-defined precincts are present the
/// maximum-precinct default `PPx = PPy = 15` applies at every resolution
/// level (T.800 Table A.13 — `Scod` bit 0 clear → "Entropy coder,
/// precincts with PPx = 15 and PPy = 15").
///
/// `PPx` / `PPy` may be zero only at the resolution level corresponding
/// to the `NLLL` band (`r = 0`); at every `r ≥ 1` they are at least 1
/// (Table A.21). This is an encoder constraint — the partition formula
/// here does not enforce it (a malformed `PPx = 0` at `r > 0` simply
/// yields a one-sample-wide precinct grid).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrecinctExponents {
    /// `PPx` — precinct width exponent (precinct width = `2^PPx`).
    pub ppx: u8,
    /// `PPy` — precinct height exponent (precinct height = `2^PPy`).
    pub ppy: u8,
}

/// The number of precincts spanning one tile-component resolution level,
/// per T.800 §B.6 / Equation B-16.
///
/// The precinct partition is anchored at `(0, 0)` on the reduced-
/// resolution tile-component domain, so a precinct's upper-left corner
/// sits at an integer multiple of `(2^PPx, 2^PPy)`. Equation B-16 counts
/// how many such anchored cells the resolution-level rectangle
/// `[trx0, trx1) × [try0, try1)` overlaps:
///
/// ```text
/// numprecinctswide = ceil(trx1 / 2^PPx) - floor(trx0 / 2^PPx)   if trx1 > trx0, else 0
/// numprecinctshigh = ceil(try1 / 2^PPy) - floor(try0 / 2^PPy)   if try1 > try0, else 0
/// numprecincts     = numprecinctswide * numprecinctshigh
/// ```
///
/// `numprecincts` may be zero (when the resolution level is empty), and
/// even when both dimensions are non-zero individual precincts may turn
/// out empty after the §B.7 code-block partition (no sub-band
/// coefficients land in them); §B.6 still requires every such precinct
/// to be represented by a (possibly empty) packet (§B.9). The precinct
/// index runs `0..numprecincts` in raster order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrecinctPartition {
    /// Precinct exponents `(PPx, PPy)` used for this resolution level.
    pub exponents: PrecinctExponents,
    /// `numprecinctswide` per Equation B-16.
    pub num_wide: u32,
    /// `numprecinctshigh` per Equation B-16.
    pub num_high: u32,
}

impl PrecinctPartition {
    /// `numprecincts = numprecinctswide * numprecinctshigh` per §B.6.
    ///
    /// Widened to `u64` because both factors can in principle reach the
    /// `u32` range for a maximal tile-component with small `PP`.
    pub fn num_precincts(&self) -> u64 {
        self.num_wide as u64 * self.num_high as u64
    }
}

/// The effective code-block dimensions for the sub-bands at one
/// resolution level, per T.800 §B.7 / Equation B-17 / Equation B-18.
///
/// The nominal code-block size signalled in `COD` / `COC` is `2^xcb` by
/// `2^ycb` (Table A.18: the stored byte is `xcb - 2`, i.e. the real
/// exponent is the stored value `+ 2`). At each resolution level the
/// effective exponents are clamped down so a code-block never exceeds the
/// precinct:
///
/// ```text
/// xcb' = min(xcb, PPx - 1)  for r = 0,   min(xcb, PPx)  for r > 0
/// ycb' = min(ycb, PPy - 1)  for r = 0,   min(ycb, PPy)  for r > 0
/// ```
///
/// The code-block partition is, like the precinct partition, anchored at
/// `(0, 0)`: the first column of code-blocks starts at `x = n·2^xcb'`,
/// the first row at `y = m·2^ycb'` (`m`, `n` integers). A code-block may
/// extend past the sub-band edge; only the coefficients inside the
/// sub-band are coded (§B.7 NOTE).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeBlockDimensions {
    /// `xcb'` — effective code-block width exponent (width = `2^xcb'`).
    pub xcb: u8,
    /// `ycb'` — effective code-block height exponent (height = `2^ycb'`).
    pub ycb: u8,
}

impl CodeBlockDimensions {
    /// Nominal code-block width `2^xcb'` (`xcb'` is at most 10, so the
    /// result fits comfortably in `u32`).
    pub fn width(&self) -> u32 {
        1u32 << self.xcb
    }

    /// Nominal code-block height `2^ycb'`.
    pub fn height(&self) -> u32 {
        1u32 << self.ycb
    }
}

/// Look up the precinct exponents `(PPx, PPy)` in force at resolution
/// level `r`, given the `COD` / `COC` precinct byte vector.
///
/// `precincts` is the raw `Vec<u8>` from a parsed `COD` / `COC`:
///
/// * **Empty** → maximum-precinct mode (no user-defined precincts): the
///   default `PPx = PPy = 15` applies at every resolution level (T.800
///   Table A.13).
/// * **Non-empty** → one byte per resolution level in order, the first
///   byte for `r = 0` (the `NLLL` band). Per Table A.21 the low nibble
///   is `PPx` and the high nibble is `PPy`. If `r` is past the end of
///   the vector (a malformed marker that signalled fewer than `NL + 1`
///   bytes) the last byte is reused — but a well-formed `COD` always
///   carries exactly `NL + 1` bytes (Table A.21), so the test corpus
///   never hits the fallback.
pub fn precinct_exponents_at(precincts: &[u8], r: u8) -> PrecinctExponents {
    if precincts.is_empty() {
        return PrecinctExponents { ppx: 15, ppy: 15 };
    }
    let idx = (r as usize).min(precincts.len() - 1);
    let byte = precincts[idx];
    PrecinctExponents {
        ppx: byte & 0x0F,
        ppy: (byte >> 4) & 0x0F,
    }
}

/// Floor of `value / 2^exp` for a `u32` and an exponent `0..=31`.
#[inline]
fn floor_div_pow2(value: u32, exp: u8) -> u32 {
    if exp >= 32 {
        0
    } else {
        value >> exp
    }
}

/// Ceiling of `value / 2^exp` for a `u32` and an exponent `0..=31`.
#[inline]
fn ceil_div_pow2_exp(value: u32, exp: u8) -> u32 {
    ceil_div_pow2(value, exp as u32)
}

/// Derive the precinct partition for one resolution level per T.800 §B.6
/// / Equation B-16.
///
/// `level` is the [`ResolutionLevel`] (its `(trx0, try0, trx1, try1)`
/// rectangle drives the count) and `exponents` is the `(PPx, PPy)` in
/// force at that level (see [`precinct_exponents_at`]).
///
/// Returns a [`PrecinctPartition`] carrying `numprecinctswide`,
/// `numprecinctshigh`, and the exponents. An empty resolution level
/// (`trx1 == trx0` or `try1 == try0`) yields a count of zero on that
/// axis per Equation B-16's case split.
pub fn derive_precinct_partition(
    level: &ResolutionLevel,
    exponents: PrecinctExponents,
) -> PrecinctPartition {
    let num_wide = if level.trx1 > level.trx0 {
        ceil_div_pow2_exp(level.trx1, exponents.ppx) - floor_div_pow2(level.trx0, exponents.ppx)
    } else {
        0
    };
    let num_high = if level.try1 > level.try0 {
        ceil_div_pow2_exp(level.try1, exponents.ppy) - floor_div_pow2(level.try0, exponents.ppy)
    } else {
        0
    };
    PrecinctPartition {
        exponents,
        num_wide,
        num_high,
    }
}

/// Derive the effective code-block dimensions at resolution level `r`
/// per T.800 §B.7 / Equation B-17 / Equation B-18.
///
/// `xcb` / `ycb` are the **real** code-block exponents (i.e. the
/// `COD` / `COC` stored byte `+ 2` per Table A.18 — the caller adds the
/// `+ 2`; this function does the §B.7 clamp only). `r` is the resolution
/// level and `exponents` is the `(PPx, PPy)` in force at that level.
///
/// ```text
/// xcb' = min(xcb, PPx - 1)  at r = 0,   min(xcb, PPx)  at r > 0
/// ycb' = min(ycb, PPy - 1)  at r = 0,   min(ycb, PPy)  at r > 0
/// ```
///
/// `PPx - 1` / `PPy - 1` at `r = 0` use a saturating subtraction so a
/// `PP = 0` (legal only at `r = 0`, Table A.21) clamps the effective
/// exponent to zero (a `1×n` / `n×1` code-block partition) rather than
/// underflowing.
pub fn derive_code_block_dimensions(
    r: u8,
    xcb: u8,
    ycb: u8,
    exponents: PrecinctExponents,
) -> CodeBlockDimensions {
    let (px_bound, py_bound) = if r == 0 {
        (
            exponents.ppx.saturating_sub(1),
            exponents.ppy.saturating_sub(1),
        )
    } else {
        (exponents.ppx, exponents.ppy)
    };
    CodeBlockDimensions {
        xcb: xcb.min(px_bound),
        ycb: ycb.min(py_bound),
    }
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

    // -----------------------------------------------------------------
    // Resolution-level + sub-band geometry (Equation B-14 / B-15 / Table
    // B.1).
    // -----------------------------------------------------------------

    #[test]
    fn resolution_level_count_is_nl_plus_one() {
        // §B.5: "there are NL + 1 distinct resolution levels, denoted
        // r = 0, 1, ..., NL".
        let tc = TileComponentGeometry {
            tcx0: 0,
            tcy0: 0,
            tcx1: 128,
            tcy1: 128,
        };
        for n_l in 0u8..=5 {
            let levels = derive_resolution_levels(tc, n_l);
            assert_eq!(levels.len(), n_l as usize + 1, "NL = {n_l}");
            for (idx, lvl) in levels.iter().enumerate() {
                assert_eq!(lvl.r, idx as u8);
                assert_eq!(lvl.n_l, n_l);
            }
        }
    }

    #[test]
    fn resolution_level_zero_carries_only_ll_band() {
        // §B.5 lead-in: "The lowest resolution level, r = 0, is
        // represented by the NLLL band."
        let tc = TileComponentGeometry {
            tcx0: 0,
            tcy0: 0,
            tcx1: 64,
            tcy1: 64,
        };
        let levels = derive_resolution_levels(tc, 3);
        let r0 = &levels[0];
        assert_eq!(r0.sub_bands.len(), 1);
        assert_eq!(r0.sub_bands[0].orientation, SubBandOrientation::LL);
        // nb = NL for the r = 0 LL band.
        assert_eq!(r0.sub_bands[0].nb, 3);
    }

    #[test]
    fn resolution_levels_above_zero_carry_three_high_pass_bands() {
        // §B.5: "Each resolution level consists of either the HL, LH
        // and HH sub-bands from one decomposition level or the NLLL
        // sub-band." For r ≥ 1, the resolution level holds the three
        // high-pass bands.
        let tc = TileComponentGeometry {
            tcx0: 0,
            tcy0: 0,
            tcx1: 64,
            tcy1: 64,
        };
        let levels = derive_resolution_levels(tc, 3);
        for r in 1u8..=3 {
            let lvl = &levels[r as usize];
            assert_eq!(lvl.sub_bands.len(), 3, "r = {r}");
            assert_eq!(
                lvl.sub_bands[0].orientation,
                SubBandOrientation::HL,
                "r = {r}"
            );
            assert_eq!(
                lvl.sub_bands[1].orientation,
                SubBandOrientation::LH,
                "r = {r}"
            );
            assert_eq!(
                lvl.sub_bands[2].orientation,
                SubBandOrientation::HH,
                "r = {r}"
            );
            // nb = NL - r + 1 at every r ≥ 1.
            for sb in &lvl.sub_bands {
                assert_eq!(sb.nb, 3 - r + 1, "r = {r} orientation {:?}", sb.orientation);
            }
        }
    }

    #[test]
    fn resolution_level_corners_halve_each_step() {
        // Eq B-14 denominator is 2^(NL - r). For a 64×64 tile-component
        // with NL = 3:
        //   r = 0: 2^3 = 8     → 8×8
        //   r = 1: 2^2 = 4     → 16×16
        //   r = 2: 2^1 = 2     → 32×32
        //   r = 3: 2^0 = 1     → 64×64 (full)
        let tc = TileComponentGeometry {
            tcx0: 0,
            tcy0: 0,
            tcx1: 64,
            tcy1: 64,
        };
        let levels = derive_resolution_levels(tc, 3);
        let expected_dims = [(8u32, 8u32), (16, 16), (32, 32), (64, 64)];
        for (r, (w, h)) in expected_dims.iter().enumerate() {
            let lvl = &levels[r];
            assert_eq!(lvl.width(), *w, "r = {r}");
            assert_eq!(lvl.height(), *h, "r = {r}");
        }
    }

    #[test]
    fn resolution_level_r_equals_nl_matches_tile_component() {
        // Eq B-14 with r = NL has denominator 1, so the highest
        // resolution level's rectangle equals the tile-component
        // rectangle exactly.
        let tc = TileComponentGeometry {
            tcx0: 17,
            tcy0: 19,
            tcx1: 250,
            tcy1: 333,
        };
        let levels = derive_resolution_levels(tc, 4);
        let top = levels.last().unwrap();
        assert_eq!(top.r, 4);
        assert_eq!(top.trx0, 17);
        assert_eq!(top.try0, 19);
        assert_eq!(top.trx1, 250);
        assert_eq!(top.try1, 333);
    }

    #[test]
    fn resolution_level_corner_uses_ceiling_division() {
        // Eq B-14: ceil(tc / 2^(NL - r)). Pick odd tc bounds so the
        // ceiling shows up.
        //   tcx0 = 1, NL = 1, r = 0 → ceil(1 / 2) = 1.
        //   tcx1 = 5, NL = 1, r = 0 → ceil(5 / 2) = 3.
        let tc = TileComponentGeometry {
            tcx0: 1,
            tcy0: 1,
            tcx1: 5,
            tcy1: 5,
        };
        let levels = derive_resolution_levels(tc, 1);
        let r0 = &levels[0];
        assert_eq!(r0.trx0, 1);
        assert_eq!(r0.try0, 1);
        assert_eq!(r0.trx1, 3);
        assert_eq!(r0.try1, 3);
        // r = NL = 1 is the identity.
        let r1 = &levels[1];
        assert_eq!(r1.trx0, 1);
        assert_eq!(r1.trx1, 5);
    }

    #[test]
    fn subband_orientation_displacements_match_table_b1() {
        // Table B.1:
        //   nbLL  → (xob, yob) = (0, 0)
        //   nbHL  → (xob, yob) = (1, 0)
        //   nbLH  → (xob, yob) = (0, 1)
        //   nbHH  → (xob, yob) = (1, 1)
        assert_eq!(SubBandOrientation::LL.xob(), 0);
        assert_eq!(SubBandOrientation::LL.yob(), 0);
        assert_eq!(SubBandOrientation::HL.xob(), 1);
        assert_eq!(SubBandOrientation::HL.yob(), 0);
        assert_eq!(SubBandOrientation::LH.xob(), 0);
        assert_eq!(SubBandOrientation::LH.yob(), 1);
        assert_eq!(SubBandOrientation::HH.xob(), 1);
        assert_eq!(SubBandOrientation::HH.yob(), 1);
    }

    #[test]
    fn subband_corners_match_eq_b15_aligned_tile() {
        // Aligned tile-component: tcx0 = tcy0 = 0, tcx1 = tcy1 = 64,
        // NL = 1. At r = 1, nb = 1, 2^nb = 2, 2^(nb-1) = 1.
        //   HL: (xob, yob) = (1, 0).
        //     tbx0 = ceil((0 - 1*1) / 2) = ceil(-1/2) → clamped to 0.
        //     tby0 = ceil((0 - 1*0) / 2) = 0.
        //     tbx1 = ceil((64 - 1*1) / 2) = ceil(63/2) = 32.
        //     tby1 = ceil((64 - 1*0) / 2) = 32.
        //   LH: symmetric in x / y.
        //   HH: tbx0 = clamped 0, tby0 = clamped 0,
        //     tbx1 = 32, tby1 = 32.
        let tc = TileComponentGeometry {
            tcx0: 0,
            tcy0: 0,
            tcx1: 64,
            tcy1: 64,
        };
        let levels = derive_resolution_levels(tc, 1);
        let r1 = &levels[1];
        let hl = r1
            .sub_bands
            .iter()
            .find(|s| s.orientation == SubBandOrientation::HL)
            .unwrap();
        assert_eq!(hl.tbx0, 0);
        assert_eq!(hl.tby0, 0);
        assert_eq!(hl.tbx1, 32);
        assert_eq!(hl.tby1, 32);

        let lh = r1
            .sub_bands
            .iter()
            .find(|s| s.orientation == SubBandOrientation::LH)
            .unwrap();
        assert_eq!(lh.tbx0, 0);
        assert_eq!(lh.tby0, 0);
        assert_eq!(lh.tbx1, 32);
        assert_eq!(lh.tby1, 32);

        let hh = r1
            .sub_bands
            .iter()
            .find(|s| s.orientation == SubBandOrientation::HH)
            .unwrap();
        assert_eq!(hh.tbx0, 0);
        assert_eq!(hh.tby0, 0);
        assert_eq!(hh.tbx1, 32);
        assert_eq!(hh.tby1, 32);
    }

    #[test]
    fn subband_dimensions_sum_to_resolution_above() {
        // For an aligned tile with even dims, the three high-pass bands
        // at resolution r plus the r-1 resolution level (LL at the
        // lower resolution) tile up the r resolution. With 64×64 at
        // NL = 1:
        //   r=0 LL = 32×32, r=1 HL/LH/HH = 32×32 each, full = 64×64.
        let tc = TileComponentGeometry {
            tcx0: 0,
            tcy0: 0,
            tcx1: 64,
            tcy1: 64,
        };
        let levels = derive_resolution_levels(tc, 1);
        let r0 = &levels[0];
        let r1 = &levels[1];
        assert_eq!(r0.width(), 32);
        assert_eq!(r0.height(), 32);
        // r0 LL should be 32x32.
        assert_eq!(r0.sub_bands[0].width(), 32);
        assert_eq!(r0.sub_bands[0].height(), 32);
        // r1's three sub-bands are each 32x32.
        for sb in &r1.sub_bands {
            assert_eq!(sb.width(), 32);
            assert_eq!(sb.height(), 32);
        }
        // r1 resolution-level rectangle is the full tile-component.
        assert_eq!(r1.width(), 64);
        assert_eq!(r1.height(), 64);
    }

    #[test]
    fn resolution_levels_with_nl_zero_emit_single_level_with_ll() {
        // NL = 0 corner: zero decomposition levels means there is no
        // wavelet transform. The single resolution level is r = 0 and
        // it carries one LL band identical to the tile-component.
        let tc = TileComponentGeometry {
            tcx0: 0,
            tcy0: 0,
            tcx1: 16,
            tcy1: 16,
        };
        let levels = derive_resolution_levels(tc, 0);
        assert_eq!(levels.len(), 1);
        let r0 = &levels[0];
        assert_eq!(r0.r, 0);
        assert_eq!(r0.n_l, 0);
        assert_eq!(r0.trx0, 0);
        assert_eq!(r0.try0, 0);
        assert_eq!(r0.trx1, 16);
        assert_eq!(r0.try1, 16);
        assert_eq!(r0.sub_bands.len(), 1);
        assert_eq!(r0.sub_bands[0].orientation, SubBandOrientation::LL);
        // nb = NL = 0 at the no-decomp corner.
        assert_eq!(r0.sub_bands[0].nb, 0);
    }

    #[test]
    fn subband_offset_tile_component_uses_signed_corner_math() {
        // Tile-component with a non-zero origin: tcx0 = 1, tcy0 = 1,
        // tcx1 = 5, tcy1 = 5 (a 4×4 tile-component offset by 1 on each
        // axis), NL = 1.
        //
        // r = 1, nb = 1, 2^nb = 2, 2^(nb-1) = 1.
        //   HL: xob = 1, yob = 0.
        //     tbx0 = ceil((1 - 1) / 2) = ceil(0/2) = 0.
        //     tby0 = ceil((1 - 0) / 2) = ceil(1/2) = 1.
        //     tbx1 = ceil((5 - 1) / 2) = ceil(4/2) = 2.
        //     tby1 = ceil((5 - 0) / 2) = ceil(5/2) = 3.
        //   LH: xob = 0, yob = 1.
        //     tbx0 = 1, tby0 = 0, tbx1 = 3, tby1 = 2.
        //   HH: xob = 1, yob = 1.
        //     tbx0 = 0, tby0 = 0, tbx1 = 2, tby1 = 2.
        let tc = TileComponentGeometry {
            tcx0: 1,
            tcy0: 1,
            tcx1: 5,
            tcy1: 5,
        };
        let levels = derive_resolution_levels(tc, 1);
        let r1 = &levels[1];
        let hl = &r1.sub_bands[0];
        assert_eq!(hl.orientation, SubBandOrientation::HL);
        assert_eq!((hl.tbx0, hl.tby0, hl.tbx1, hl.tby1), (0, 1, 2, 3));

        let lh = &r1.sub_bands[1];
        assert_eq!(lh.orientation, SubBandOrientation::LH);
        assert_eq!((lh.tbx0, lh.tby0, lh.tbx1, lh.tby1), (1, 0, 3, 2));

        let hh = &r1.sub_bands[2];
        assert_eq!(hh.orientation, SubBandOrientation::HH);
        assert_eq!((hh.tbx0, hh.tby0, hh.tbx1, hh.tby1), (0, 0, 2, 2));
    }

    #[test]
    fn subband_max_nl_does_not_panic() {
        // NL = 32 is the Table A.15 upper bound. ceil_div_pow2 with
        // n = 32 must not overflow.
        let tc = TileComponentGeometry {
            tcx0: 0,
            tcy0: 0,
            tcx1: u32::MAX,
            tcy1: u32::MAX,
        };
        let levels = derive_resolution_levels(tc, 32);
        assert_eq!(levels.len(), 33);
        // r = 0 with denominator 2^32: ceil((2^32 - 1) / 2^32) = 1.
        let r0 = &levels[0];
        assert_eq!(r0.trx0, 0);
        assert_eq!(r0.try0, 0);
        assert_eq!(r0.trx1, 1);
        assert_eq!(r0.try1, 1);
    }

    // -----------------------------------------------------------------
    // End resolution-level / sub-band tests.
    // -----------------------------------------------------------------

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

    // -- §B.6 precinct partition (Eq B-16) -----------------------------

    #[test]
    fn precinct_exponents_default_to_max_when_empty() {
        // Table A.13: Scod bit 0 clear → "precincts with PPx = 15 and
        // PPy = 15" at every resolution level (maximum-precinct mode).
        for r in 0u8..=8 {
            let pp = precinct_exponents_at(&[], r);
            assert_eq!(pp.ppx, 15, "r = {r}");
            assert_eq!(pp.ppy, 15, "r = {r}");
        }
    }

    #[test]
    fn precinct_exponents_decode_table_a21_nibbles() {
        // Table A.21: low nibble = PPx, high nibble = PPy. Two bytes:
        // r = 0 → 0x54 (PPx = 4, PPy = 5); r = 1 → 0x76 (PPx = 6,
        // PPy = 7).
        let precincts = [0x54u8, 0x76u8];
        let r0 = precinct_exponents_at(&precincts, 0);
        assert_eq!(r0.ppx, 4);
        assert_eq!(r0.ppy, 5);
        let r1 = precinct_exponents_at(&precincts, 1);
        assert_eq!(r1.ppx, 6);
        assert_eq!(r1.ppy, 7);
    }

    #[test]
    fn precinct_count_aligned_tile_component() {
        // Eq B-16. Aligned 64×64 tile-component, NL = 1, precinct
        // exponents PPx = PPy = 4 (precinct = 16×16):
        //   r = 0: rect [0, 32) → ceil(32/16) - floor(0/16) = 2 wide,
        //          2 high → 4 precincts.
        //   r = 1: rect [0, 64) → ceil(64/16) - floor(0/16) = 4 wide,
        //          4 high → 16 precincts.
        let tc = TileComponentGeometry {
            tcx0: 0,
            tcy0: 0,
            tcx1: 64,
            tcy1: 64,
        };
        let levels = derive_resolution_levels(tc, 1);
        let pp = PrecinctExponents { ppx: 4, ppy: 4 };

        let p0 = derive_precinct_partition(&levels[0], pp);
        assert_eq!(p0.num_wide, 2);
        assert_eq!(p0.num_high, 2);
        assert_eq!(p0.num_precincts(), 4);

        let p1 = derive_precinct_partition(&levels[1], pp);
        assert_eq!(p1.num_wide, 4);
        assert_eq!(p1.num_high, 4);
        assert_eq!(p1.num_precincts(), 16);
    }

    #[test]
    fn precinct_count_offset_tile_component_uses_floor_of_origin() {
        // Eq B-16 subtracts floor(trx0 / 2^PPx), not ceil. A
        // tile-component anchored away from (0, 0) can straddle one more
        // precinct cell than its width/2^PP alone implies. NL = 0 so the
        // single resolution level r = 0 equals the tile-component
        // rectangle [20, 50) × [0, 16). PPx = PPy = 4 (16×16 precinct):
        //   wide: ceil(50/16) - floor(20/16) = 4 - 1 = 3
        //   high: ceil(16/16) - floor(0/16)  = 1 - 0 = 1
        let tc = TileComponentGeometry {
            tcx0: 20,
            tcy0: 0,
            tcx1: 50,
            tcy1: 16,
        };
        let levels = derive_resolution_levels(tc, 0);
        let pp = PrecinctExponents { ppx: 4, ppy: 4 };
        let part = derive_precinct_partition(&levels[0], pp);
        assert_eq!(part.num_wide, 3);
        assert_eq!(part.num_high, 1);
        assert_eq!(part.num_precincts(), 3);
    }

    #[test]
    fn precinct_count_max_precincts_is_single_precinct() {
        // PPx = PPy = 15 → precinct 32768×32768. A modest tile-component
        // fits in one precinct per resolution level (numprecincts = 1).
        let tc = TileComponentGeometry {
            tcx0: 0,
            tcy0: 0,
            tcx1: 256,
            tcy1: 256,
        };
        let levels = derive_resolution_levels(tc, 3);
        for lvl in &levels {
            let pp = precinct_exponents_at(&[], lvl.r);
            let part = derive_precinct_partition(lvl, pp);
            assert_eq!(part.num_precincts(), 1, "r = {}", lvl.r);
        }
    }

    #[test]
    fn precinct_count_empty_resolution_level_is_zero() {
        // Eq B-16 case split: if trx1 == trx0 the wide count is 0 (and
        // numprecincts is 0). A degenerate zero-width tile-component
        // exercises the branch.
        let tc = TileComponentGeometry {
            tcx0: 10,
            tcy0: 10,
            tcx1: 10,
            tcy1: 20,
        };
        let levels = derive_resolution_levels(tc, 0);
        let pp = PrecinctExponents { ppx: 4, ppy: 4 };
        let part = derive_precinct_partition(&levels[0], pp);
        assert_eq!(part.num_wide, 0);
        assert!(part.num_high > 0);
        assert_eq!(part.num_precincts(), 0);
    }

    // -- §B.7 code-block partition (Eq B-17 / B-18) --------------------

    #[test]
    fn code_block_dims_unclamped_when_precinct_is_large() {
        // Eq B-17 / B-18 at r > 0: xcb' = min(xcb, PPx). When PPx > xcb
        // the code-block keeps its nominal exponent. xcb = ycb = 6
        // (64×64 code-block), PPx = PPy = 15 → xcb' = ycb' = 6.
        let pp = PrecinctExponents { ppx: 15, ppy: 15 };
        let cb = derive_code_block_dimensions(2, 6, 6, pp);
        assert_eq!(cb.xcb, 6);
        assert_eq!(cb.ycb, 6);
        assert_eq!(cb.width(), 64);
        assert_eq!(cb.height(), 64);
    }

    #[test]
    fn code_block_dims_clamped_to_precinct_above_r_zero() {
        // Eq B-17 / B-18 at r > 0: xcb' = min(xcb, PPx). PPx = PPy = 4,
        // nominal xcb = ycb = 6 → xcb' = ycb' = min(6, 4) = 4 (16×16).
        let pp = PrecinctExponents { ppx: 4, ppy: 4 };
        let cb = derive_code_block_dimensions(1, 6, 6, pp);
        assert_eq!(cb.xcb, 4);
        assert_eq!(cb.ycb, 4);
        assert_eq!(cb.width(), 16);
        assert_eq!(cb.height(), 16);
    }

    #[test]
    fn code_block_dims_use_pp_minus_one_at_r_zero() {
        // Eq B-17 / B-18 at r = 0: xcb' = min(xcb, PPx - 1). PPx =
        // PPy = 4, nominal xcb = ycb = 6 → xcb' = ycb' = min(6, 3) = 3
        // (8×8). The r = 0 case shaves one off PPx/PPy.
        let pp = PrecinctExponents { ppx: 4, ppy: 4 };
        let cb = derive_code_block_dimensions(0, 6, 6, pp);
        assert_eq!(cb.xcb, 3);
        assert_eq!(cb.ycb, 3);
        assert_eq!(cb.width(), 8);
        assert_eq!(cb.height(), 8);
    }

    #[test]
    fn code_block_dims_pp_zero_at_r_zero_saturates() {
        // Table A.21 allows PPx = PPy = 0 only at the NLLL band (r = 0).
        // Eq B-17 then gives xcb' = min(xcb, PPx - 1) = min(xcb, 0)
        // under saturating subtraction → 0 (a 1×1 code-block partition),
        // not a wraparound to a giant block.
        let pp = PrecinctExponents { ppx: 0, ppy: 0 };
        let cb = derive_code_block_dimensions(0, 6, 6, pp);
        assert_eq!(cb.xcb, 0);
        assert_eq!(cb.ycb, 0);
        assert_eq!(cb.width(), 1);
        assert_eq!(cb.height(), 1);
    }

    #[test]
    fn code_block_dims_asymmetric_exponents() {
        // xcb and ycb need not be equal, and the clamp is applied
        // independently per axis. xcb = 7, ycb = 4, PPx = 5, PPy = 6,
        // r = 2 (r > 0 branch):
        //   xcb' = min(7, 5) = 5 (32 wide)
        //   ycb' = min(4, 6) = 4 (16 high)
        let pp = PrecinctExponents { ppx: 5, ppy: 6 };
        let cb = derive_code_block_dimensions(2, 7, 4, pp);
        assert_eq!(cb.xcb, 5);
        assert_eq!(cb.ycb, 4);
        assert_eq!(cb.width(), 32);
        assert_eq!(cb.height(), 16);
    }
}
