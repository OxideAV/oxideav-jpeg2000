//! Component-local coefficient storage for immediate block reconstruction.
//!
//! Only geometry and reconstructed coefficients survive a scatter. The
//! caller can reset or drop the code-block immediately afterwards. This
//! module uses the existing OxideAV dequantisation and inverse DWT routines;
//! it changes the lifetime of block state, not their arithmetic.

#[cfg(test)]
use super::{CodedCodeBlock, check_placement};
use super::{SubBandQuantization, check_placement_shape, r_qb_to_i32};
use crate::Error;
use crate::dequant::irreversible_step_size;
use crate::dwt::{Interleaved2D, sr_2d_5x3_threaded, sr_2d_9x7_threaded};
use crate::geometry::{PrecinctCodeBlock, ResolutionLevel, SubBand, SubBandOrientation};
use crate::t1::CodeBlock;

/// The Equation E-6 / E-8 reconstruction offsets `r · 2^(Mb − Nb)`,
/// computed once per distinct shift instead of once per coefficient.
struct MidpointOffsets {
    r: f64,
    cached: [f64; 64],
}

impl MidpointOffsets {
    fn new(r: f64) -> Self {
        Self {
            r,
            cached: [f64::NAN; 64],
        }
    }

    #[inline]
    fn get(&mut self, shift: u32) -> f64 {
        match self.cached.get_mut(shift as usize) {
            Some(slot) => {
                if slot.is_nan() {
                    *slot = self.r * f64::from(shift).exp2();
                }
                *slot
            }
            None => self.r * f64::from(shift).exp2(),
        }
    }
}

/// Reconstruct a whole reversible code-block per Equation E-8 into a
/// raster-order `i32` array: `Rqb = qb` once every bit-plane was
/// decoded (`Nb ≥ Mb`), `qb ± r · 2^(Mb − Nb)` truncated toward zero
/// otherwise. Value for value this is `r_qb_to_i32(
/// reconstruct_reversible(qb, mb, Nb, r))` over the block.
pub(crate) fn reconstruct_block_5x3(block: &CodeBlock, nb: u32, mb: u32, r: f64) -> Vec<i32> {
    let mut out = Vec::with_capacity(block.width() * block.height());
    let mut offsets = MidpointOffsets::new(r);
    block.for_each_signed_with_nb(nb, |qb, nb| {
        out.push(if nb >= mb {
            // An integer inside the f64-exact range: the saturating
            // conversion is the integer clamp.
            qb.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
        } else if qb == 0 {
            0
        } else {
            let offset = offsets.get(mb - nb);
            r_qb_to_i32(qb as f64 + if qb > 0 { offset } else { -offset })
        });
    });
    out
}

/// Reconstruct a whole irreversible code-block per Equation E-6 into a
/// raster-order `f64` array — value for value
/// `reconstruct_irreversible(qb, Mb, Nb, Δb, r)` over the block.
pub(crate) fn reconstruct_block_9x7(
    block: &CodeBlock,
    nb: u32,
    quant: SubBandQuantization,
    r: f64,
) -> Vec<f64> {
    let step = irreversible_step_size(quant.rb, quant.step);
    let mut out = Vec::with_capacity(block.width() * block.height());
    let mut offsets = MidpointOffsets::new(r);
    block.for_each_signed_with_nb(nb, |qb, nb| {
        out.push(if qb == 0 {
            0.0
        } else {
            let offset = offsets.get(quant.mb.saturating_sub(nb));
            (qb as f64 + if qb > 0 { offset } else { -offset }) * step
        });
    });
    out
}

struct BandRegion<T> {
    band: SubBand,
    offset: usize,
    data: Vec<T>,
}

/// Each coded band owns a contiguous coefficient buffer. Uncoded positions
/// stay zero. The shared overlap guard costs one bit per sample and is
/// released before synthesis; no decoded block is retained here. Synthesis
/// moves LL into the carry and releases each level's high bands immediately.
pub(crate) struct CoefficientSurface<'a, T> {
    levels: &'a [ResolutionLevel],
    bands: Vec<Vec<BandRegion<T>>>,
    written: Vec<u64>,
}

type Inverse2D<T> = fn(
    &[T],
    (usize, usize),
    &[T],
    (usize, usize),
    &[T],
    (usize, usize),
    &[T],
    (usize, usize),
    i32,
    i32,
    usize,
) -> Result<Interleaved2D<T>, Error>;

impl<'a, T: Default + Clone> CoefficientSurface<'a, T> {
    pub(crate) fn new(levels: &'a [ResolutionLevel]) -> Result<Self, Error> {
        if levels.is_empty() {
            return Err(Error::InvalidMarkerLength);
        }
        let mut count = 0usize;
        let mut layout = Vec::with_capacity(levels.len());
        for (resolution, level) in levels.iter().enumerate() {
            let orientations: &[SubBandOrientation] = if resolution == 0 {
                &[SubBandOrientation::LL]
            } else {
                &[
                    SubBandOrientation::HL,
                    SubBandOrientation::LH,
                    SubBandOrientation::HH,
                ]
            };
            if level.r as usize != resolution || level.sub_bands.len() != orientations.len() {
                return Err(Error::InvalidMarkerLength);
            }
            let mut regions = Vec::with_capacity(orientations.len());
            for (&band, &orientation) in level.sub_bands.iter().zip(orientations) {
                if band.orientation != orientation {
                    return Err(Error::InvalidMarkerLength);
                }
                let len = (band.width() as usize)
                    .checked_mul(band.height() as usize)
                    .ok_or(Error::InvalidMarkerLength)?;
                let offset = count;
                count = count.checked_add(len).ok_or(Error::InvalidMarkerLength)?;
                regions.push((band, offset, len));
            }
            layout.push(regions);
        }
        // Validate every level before allocating any coefficient buffer. A
        // malformed later band must not cause a large earlier allocation.
        let bands = layout
            .into_iter()
            .map(|regions| {
                regions
                    .into_iter()
                    .map(|(band, offset, len)| BandRegion {
                        band,
                        offset,
                        data: vec![T::default(); len],
                    })
                    .collect()
            })
            .collect();
        Ok(Self {
            levels,
            bands,
            written: vec![0; count.div_ceil(64)],
        })
    }

    /// Place one reconstructed code-block (`values` in raster order,
    /// `width × height` as the placement states) into its sub-band.
    /// Rejects a placement outside the band, a shape or orientation
    /// mismatch, and any sample another block already wrote.
    pub(crate) fn scatter_values(
        &mut self,
        resolution: usize,
        sub_band: usize,
        placement: &PrecinctCodeBlock,
        orientation: SubBandOrientation,
        values: &[T],
    ) -> Result<(), Error> {
        let region = self
            .bands
            .get_mut(resolution)
            .and_then(|bands| bands.get_mut(sub_band))
            .ok_or(Error::InvalidPacketHeader)?;
        let block_w = placement.x1.saturating_sub(placement.x0) as usize;
        let block_h = placement.y1.saturating_sub(placement.y0) as usize;
        check_placement_shape(&region.band, placement, block_w, block_h, orientation)?;
        if values.len() != block_w * block_h {
            return Err(Error::InvalidMarkerLength);
        }
        if block_w == 0 {
            return Ok(());
        }
        let width = region.band.width() as usize;
        let dx = (placement.x0 - region.band.tbx0) as usize;
        let dy = (placement.y0 - region.band.tby0) as usize;
        for (v, row) in values.chunks_exact(block_w).enumerate() {
            let target = (dy + v) * width + dx;
            claim_bits(&mut self.written, region.offset + target, block_w)?;
            region.data[target..target + block_w].clone_from_slice(row);
        }
        Ok(())
    }

    fn synthesize(self, inverse: Inverse2D<T>, threads: usize) -> Result<Interleaved2D<T>, Error> {
        let Self {
            levels,
            bands,
            written,
        } = self;
        // No more scatters are possible once synthesis owns the buffers.
        drop(written);
        let mut bands = bands.into_iter();
        let ll = bands.next().unwrap().pop().unwrap();
        let mut carry_dims = (ll.band.width() as usize, ll.band.height() as usize);
        // The lowest band is already a contiguous owned buffer. Move it
        // directly instead of allocating a second copy for the first carry.
        let mut carry = ll.data;
        for (level, regions) in levels.iter().skip(1).zip(bands) {
            let mut regions = regions.into_iter();
            let hl = regions.next().unwrap();
            let lh = regions.next().unwrap();
            let hh = regions.next().unwrap();
            // These are exactly the band slices and origin parities passed
            // by the existing public IDWT cascade. No precision or rounding
            // change is introduced by the lifetime of these buffers.
            let grid = inverse(
                &carry,
                carry_dims,
                &hl.data,
                (hl.band.width() as usize, hl.band.height() as usize),
                &lh.data,
                (lh.band.width() as usize, lh.band.height() as usize),
                &hh.data,
                (hh.band.width() as usize, hh.band.height() as usize),
                crate::dwt::origin_parity(level.trx0),
                crate::dwt::origin_parity(level.try0),
                threads,
            )?;
            // Later resolutions need only the new LL carry, never these
            // coded bands. Release them before the next synthesis step.
            drop(hl);
            drop(lh);
            drop(hh);
            carry_dims = (grid.width, grid.height);
            carry = grid.data;
        }
        Ok(Interleaved2D {
            data: carry,
            width: carry_dims.0,
            height: carry_dims.1,
        })
    }
}

/// Mark `len` consecutive coverage bits starting at `first`, failing
/// if any of them was already set (two blocks claim one sample).
fn claim_bits(written: &mut [u64], first: usize, len: usize) -> Result<(), Error> {
    let mut bit = first;
    let end = first + len;
    while bit < end {
        let in_word = bit % 64;
        let take = (64 - in_word).min(end - bit);
        let mask = if take == 64 {
            u64::MAX
        } else {
            ((1u64 << take) - 1) << in_word
        };
        let word = &mut written[bit / 64];
        if *word & mask != 0 {
            return Err(Error::InvalidMarkerLength);
        }
        *word |= mask;
        bit += take;
    }
    Ok(())
}

impl CoefficientSurface<'_, i32> {
    #[cfg(test)]
    pub(crate) fn scatter_5x3(
        &mut self,
        resolution: usize,
        sub_band: usize,
        cb: CodedCodeBlock<'_>,
        mb: u32,
        r: f64,
    ) -> Result<(), Error> {
        self.check_block(resolution, sub_band, &cb)?;
        let values = reconstruct_block_5x3(cb.coefficients, cb.nb, mb, r);
        self.scatter_values(
            resolution,
            sub_band,
            &cb.placement,
            cb.coefficients.orientation(),
            &values,
        )
    }

    /// Run the §F.3 synthesis cascade over the scattered bands on up
    /// to `threads` workers.
    pub(crate) fn into_idwt_5x3(self, threads: usize) -> Result<Interleaved2D<i32>, Error> {
        self.synthesize(sr_2d_5x3_threaded, threads)
    }
}

impl CoefficientSurface<'_, f64> {
    #[cfg(test)]
    pub(crate) fn scatter_9x7(
        &mut self,
        resolution: usize,
        sub_band: usize,
        cb: CodedCodeBlock<'_>,
        quant: SubBandQuantization,
        r: f64,
    ) -> Result<(), Error> {
        self.check_block(resolution, sub_band, &cb)?;
        let values = reconstruct_block_9x7(cb.coefficients, cb.nb, quant, r);
        self.scatter_values(
            resolution,
            sub_band,
            &cb.placement,
            cb.coefficients.orientation(),
            &values,
        )
    }

    /// Run the §F.3 synthesis cascade over the scattered bands on up
    /// to `threads` workers.
    pub(crate) fn into_idwt_9x7(self, threads: usize) -> Result<Interleaved2D<f64>, Error> {
        self.synthesize(sr_2d_9x7_threaded, threads)
    }
}

#[cfg(test)]
impl<T> CoefficientSurface<'_, T> {
    /// The placement / shape validation of a still-undecoded scatter.
    fn check_block(
        &self,
        resolution: usize,
        sub_band: usize,
        cb: &CodedCodeBlock<'_>,
    ) -> Result<(), Error> {
        let region = self
            .bands
            .get(resolution)
            .and_then(|bands| bands.get(sub_band))
            .ok_or(Error::InvalidPacketHeader)?;
        check_placement(&region.band, &cb.placement, cb.coefficients)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dequant::StepSize;
    use crate::geometry::{PrecinctCodeBlock, TileComponentGeometry, derive_resolution_levels};
    use crate::reassemble::{BlockSource, idwt_5x3, idwt_9x7};
    use crate::t1::{CodeBlock, Coefficient};

    struct Oracle<'a>(Vec<(SubBand, Vec<CodedCodeBlock<'a>>)>);

    impl<'a> BlockSource<'a> for Oracle<'a> {
        fn blocks_for(&self, band: &SubBand) -> &[CodedCodeBlock<'a>] {
            &self.0.iter().find(|(b, _)| b == band).unwrap().1
        }
    }

    // Independent caller arrangement for the unchanged public reconstruction
    // path. Multiple clipped blocks and deliberate holes exercise scatter
    // ordering, zero-filled missing contributions and every band boundary.
    fn blocks_for_band(band: SubBand, mode: u32) -> Vec<(PrecinctCodeBlock, CodeBlock)> {
        let mut blocks = Vec::new();
        for y in (band.tby0..band.tby1).step_by(2) {
            for x in (band.tbx0..band.tbx1).step_by(3) {
                if (x - band.tbx0 + 2 * (y - band.tby0) + mode).is_multiple_of(5) {
                    continue;
                }
                let placement = PrecinctCodeBlock {
                    cbx: (x - band.tbx0) / 3,
                    cby: (y - band.tby0) / 2,
                    x0: x,
                    y0: y,
                    x1: x.saturating_add(3).min(band.tbx1),
                    y1: y.saturating_add(2).min(band.tby1),
                };
                let width = placement.width() as usize;
                let height = placement.height() as usize;
                let coefficients = (0..width * height)
                    .map(|i| {
                        let magnitude = ((i as u32 * 37 + x % 97 + y % 43) % 256) * 2;
                        Coefficient {
                            magnitude,
                            sigma: magnitude != 0,
                            sign: i % 3 == 1,
                            already_refined: i % 2 == 0,
                        }
                    })
                    .collect();
                let mut block =
                    CodeBlock::from_coefficients(band.orientation, width, height, coefficients);
                if mode != 0 {
                    block.set_decoded_bits_raw((0..width * height).map(|i| i as u32 % 7).collect());
                    block.set_zero_bit_planes(1);
                }
                if mode == 2 {
                    block.apply_roi_maxshift(7, 2);
                }
                blocks.push((placement, block));
            }
        }
        blocks
    }

    #[test]
    fn surface_matches_public_reconstruction_and_idwt_exactly() {
        // Odd and even origins on both sides of 2^31, up to the top of the
        // Table A.9 range (`x0 + 17` / `y0 + 13` still fit in u32).
        for (x0, y0) in [
            (0, 0),
            (3, 1),
            (0x4000_0003, 0x4000_0001),
            (0x8000_0001, 0x7FFF_FFFF),
            (0xFFFF_FFEE, 0xFFFF_FFF2),
        ] {
            for (width, height) in [(17, 13), (1, 1), (1, 9)] {
                for n_l in 0..=3 {
                    let all_levels = derive_resolution_levels(
                        TileComponentGeometry {
                            tcx0: x0,
                            tcy0: y0,
                            tcx1: x0 + width,
                            tcy1: y0 + height,
                        },
                        n_l,
                    );
                    for keep in 0..=n_l as usize {
                        let levels = &all_levels[..=keep];
                        for mode in 0..3 {
                            let owned: Vec<_> = levels
                                .iter()
                                .flat_map(|level| &level.sub_bands)
                                .map(|&band| (band, blocks_for_band(band, mode)))
                                .collect();
                            let source = Oracle(
                                owned
                                    .iter()
                                    .map(|(band, blocks)| {
                                        (
                                            *band,
                                            blocks
                                                .iter()
                                                .map(|(placement, block)| CodedCodeBlock {
                                                    placement: *placement,
                                                    coefficients: block,
                                                    nb: 5,
                                                })
                                                .collect(),
                                        )
                                    })
                                    .collect(),
                            );
                            let mut reversible = CoefficientSurface::<i32>::new(levels).unwrap();
                            let mut irreversible = CoefficientSurface::<f64>::new(levels).unwrap();
                            let mb_per_level: Vec<_> = levels
                                .iter()
                                .map(|level| vec![7; level.sub_bands.len()])
                                .collect();
                            let quant_per_level: Vec<Vec<_>> = levels
                                .iter()
                                .map(|level| {
                                    level
                                        .sub_bands
                                        .iter()
                                        .map(|band| {
                                            SubBandQuantization::resolve(
                                                8,
                                                1,
                                                band.orientation,
                                                StepSize {
                                                    epsilon: 7,
                                                    mantissa: 113 + u16::from(level.r) * 41,
                                                },
                                            )
                                            .unwrap()
                                        })
                                        .collect()
                                })
                                .collect();
                            for (resolution, level) in levels.iter().enumerate() {
                                for (sub_band, band) in level.sub_bands.iter().enumerate() {
                                    // Reverse the block order relative to the old oracle to
                                    // prove that independent placements do not depend on it.
                                    for &cb in source.blocks_for(band).iter().rev() {
                                        reversible
                                            .scatter_5x3(resolution, sub_band, cb, 7, 0.5)
                                            .unwrap();
                                        irreversible
                                            .scatter_9x7(
                                                resolution,
                                                sub_band,
                                                cb,
                                                quant_per_level[resolution][sub_band],
                                                0.5,
                                            )
                                            .unwrap();
                                    }
                                }
                            }
                            let old = idwt_5x3(levels, &source, &mb_per_level, 0.5).unwrap();
                            let new = reversible.into_idwt_5x3(1).unwrap();
                            assert_eq!((new.width, new.height), (old.width, old.height));
                            assert_eq!(new.data, old.data);
                            let old = idwt_9x7(levels, &source, &quant_per_level, 0.5).unwrap();
                            let new = irreversible.into_idwt_9x7(1).unwrap();
                            assert_eq!((new.width, new.height), (old.width, old.height));
                            assert_eq!(
                                new.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                                old.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                                "f64 differs: origin {x0}/{y0}, {width}x{height}, NL {n_l}, keep {keep}, mode {mode}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// Blocks whose placement and coefficients depend only on positions
    /// relative to the band origin, so two parity-equivalent geometries
    /// receive identical sub-band data.
    fn relative_blocks(band: SubBand) -> Vec<(PrecinctCodeBlock, CodeBlock)> {
        let mut blocks = Vec::new();
        for ry in (0..band.height()).step_by(2) {
            for rx in (0..band.width()).step_by(3) {
                let (x, y) = (band.tbx0 + rx, band.tby0 + ry);
                let placement = PrecinctCodeBlock {
                    cbx: rx / 3,
                    cby: ry / 2,
                    x0: x,
                    y0: y,
                    x1: x + 3.min(band.width() - rx),
                    y1: y + 2.min(band.height() - ry),
                };
                let width = placement.width() as usize;
                let height = placement.height() as usize;
                let coefficients = (0..width * height)
                    .map(|i| {
                        let magnitude = ((i as u32 * 37 + rx * 11 + ry * 5) % 256) * 2;
                        Coefficient {
                            magnitude,
                            sigma: magnitude != 0,
                            sign: (i as u32 + rx) % 3 == 1,
                            already_refined: false,
                        }
                    })
                    .collect();
                blocks.push((
                    placement,
                    CodeBlock::from_coefficients(band.orientation, width, height, coefficients),
                ));
            }
        }
        blocks
    }

    /// Reconstruct one tile-component through the coefficient surface
    /// (5-3 and 9-7) and the public cascade; all three must agree, and the
    /// result is returned for cross-origin comparison.
    fn reconstruct_relative(
        x0: u32,
        y0: u32,
        width: u32,
        height: u32,
        n_l: u8,
    ) -> (Interleaved2D<i32>, Vec<u64>) {
        let levels = derive_resolution_levels(
            TileComponentGeometry {
                tcx0: x0,
                tcy0: y0,
                tcx1: x0 + width,
                tcy1: y0 + height,
            },
            n_l,
        );
        let owned: Vec<_> = levels
            .iter()
            .flat_map(|level| &level.sub_bands)
            .map(|&band| (band, relative_blocks(band)))
            .collect();
        let source = Oracle(
            owned
                .iter()
                .map(|(band, blocks)| {
                    (
                        *band,
                        blocks
                            .iter()
                            .map(|(placement, block)| CodedCodeBlock {
                                placement: *placement,
                                coefficients: block,
                                nb: 5,
                            })
                            .collect(),
                    )
                })
                .collect(),
        );
        let quant = |band: &SubBand| {
            SubBandQuantization::resolve(
                8,
                1,
                band.orientation,
                StepSize {
                    epsilon: 7,
                    mantissa: 301,
                },
            )
            .unwrap()
        };
        let mut reversible = CoefficientSurface::<i32>::new(&levels).unwrap();
        let mut irreversible = CoefficientSurface::<f64>::new(&levels).unwrap();
        for (resolution, level) in levels.iter().enumerate() {
            for (sub_band, band) in level.sub_bands.iter().enumerate() {
                for &cb in source.blocks_for(band) {
                    reversible
                        .scatter_5x3(resolution, sub_band, cb, 7, 0.5)
                        .unwrap();
                    irreversible
                        .scatter_9x7(resolution, sub_band, cb, quant(band), 0.5)
                        .unwrap();
                }
            }
        }
        let mb_per_level: Vec<_> = levels
            .iter()
            .map(|level| vec![7; level.sub_bands.len()])
            .collect();
        let quant_per_level: Vec<Vec<_>> = levels
            .iter()
            .map(|level| level.sub_bands.iter().map(quant).collect())
            .collect();
        let rev = reversible.into_idwt_5x3(1).unwrap();
        assert_eq!(rev, idwt_5x3(&levels, &source, &mb_per_level, 0.5).unwrap());
        let irr = irreversible.into_idwt_9x7(1).unwrap();
        let irr_bits: Vec<u64> = irr.data.iter().map(|v| v.to_bits()).collect();
        let public = idwt_9x7(&levels, &source, &quant_per_level, 0.5).unwrap();
        assert_eq!(
            irr_bits,
            public.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
        assert_eq!((irr.width, irr.height), (rev.width, rev.height));
        (rev, irr_bits)
    }

    #[test]
    fn huge_origins_reconstruct_like_parity_equivalent_small_origins() {
        // T.800 Table A.9 lets the grid origin reach 2^32 − 1. For
        // `NL ≤ 5` every Equation B-14 / B-15 corner parity and every band
        // size depends on the origin modulo 64, so an origin and its
        // residue modulo 64 must reconstruct the same 5-3 and 9-7 arrays.
        // The resolution-level origins here sit just below and just above
        // 2^31 (e.g. `0xFFFF_FFF1 → 0x7FFF_FFF9` at level NL − 1), where a
        // truncating `as i32` origin plus the row length used to overflow.
        let origins = [
            0x8000_0001u32,
            0x8000_0000,
            0x7FFF_FFFF,
            0xFFFF_FFF0,
            0xFFFF_FFF1,
            0xFFFF_FFF5,
        ];
        for &x0 in &origins {
            for &y0 in &origins {
                for (width, height) in [(9, 7), (1, 1), (1, 9), (10, 1)] {
                    // Stay inside the u32 grid (`x0 + width <= 2^32 − 1`).
                    if x0.checked_add(width).is_none() || y0.checked_add(height).is_none() {
                        continue;
                    }
                    for n_l in 0..=5 {
                        let big = reconstruct_relative(x0, y0, width, height, n_l);
                        let small = reconstruct_relative(x0 % 64, y0 % 64, width, height, n_l);
                        assert_eq!(
                            big,
                            small,
                            "origin ({x0:#x}, {y0:#x}) vs ({}, {}), {width}x{height}, NL {n_l}",
                            x0 % 64,
                            y0 % 64
                        );
                    }
                }
            }
        }
    }

    fn one_level() -> Vec<ResolutionLevel> {
        derive_resolution_levels(
            TileComponentGeometry {
                tcx0: 3,
                tcy0: 1,
                tcx1: 7,
                tcy1: 4,
            },
            0,
        )
    }

    #[test]
    fn surface_rejects_overlap_outside_dimensions_orientation_and_address() {
        let levels = one_level();
        let block = CodeBlock::new(SubBandOrientation::LL, 2, 2);
        let placement = PrecinctCodeBlock {
            cbx: 0,
            cby: 0,
            x0: 3,
            y0: 1,
            x1: 5,
            y1: 3,
        };
        let cb = CodedCodeBlock {
            placement,
            coefficients: &block,
            nb: 7,
        };
        let mut surface = CoefficientSurface::<i32>::new(&levels).unwrap();
        surface.scatter_5x3(0, 0, cb, 7, 0.5).unwrap();
        assert!(matches!(
            surface.scatter_5x3(0, 0, cb, 7, 0.5),
            Err(Error::InvalidMarkerLength)
        ));
        let mut surface = CoefficientSurface::<i32>::new(&levels).unwrap();
        assert!(matches!(
            surface.scatter_5x3(1, 0, cb, 7, 0.5),
            Err(Error::InvalidPacketHeader)
        ));
        assert!(matches!(
            surface.scatter_5x3(0, 1, cb, 7, 0.5),
            Err(Error::InvalidPacketHeader)
        ));
        let wrong_orientation = CodeBlock::new(SubBandOrientation::HH, 2, 2);
        assert!(matches!(
            surface.scatter_5x3(
                0,
                0,
                CodedCodeBlock {
                    coefficients: &wrong_orientation,
                    ..cb
                },
                7,
                0.5
            ),
            Err(Error::InvalidMarkerLength)
        ));
        let wrong_dimensions = CodeBlock::new(SubBandOrientation::LL, 1, 2);
        assert!(matches!(
            surface.scatter_5x3(
                0,
                0,
                CodedCodeBlock {
                    coefficients: &wrong_dimensions,
                    ..cb
                },
                7,
                0.5
            ),
            Err(Error::InvalidMarkerLength)
        ));
        let outside = PrecinctCodeBlock {
            x0: 2,
            x1: 4,
            ..placement
        };
        assert!(matches!(
            surface.scatter_5x3(
                0,
                0,
                CodedCodeBlock {
                    placement: outside,
                    ..cb
                },
                7,
                0.5
            ),
            Err(Error::InvalidMarkerLength)
        ));
        // Rejected scatters did not publish coefficients into the surface.
        let grid = surface.into_idwt_5x3(1).unwrap();
        assert_eq!(grid.data, vec![0; 12]);
    }

    #[test]
    fn single_level_moves_surface_and_preserves_saturating_reconstruction() {
        let mut levels = one_level();
        levels[0].sub_bands[0].tby1 = 2;
        levels[0].try1 = 2;
        let band = levels[0].sub_bands[0];
        let coefficients = [u32::MAX, 1 << 31, 0, 3]
            .into_iter()
            .enumerate()
            .map(|(i, magnitude)| Coefficient {
                magnitude,
                sigma: magnitude != 0,
                sign: i == 1,
                already_refined: false,
            })
            .collect();
        let mut block = CodeBlock::from_coefficients(SubBandOrientation::LL, 4, 1, coefficients);
        let cb = CodedCodeBlock {
            placement: PrecinctCodeBlock {
                cbx: 0,
                cby: 0,
                x0: band.tbx0,
                y0: band.tby0,
                x1: band.tbx1,
                y1: band.tby1,
            },
            coefficients: &block,
            nb: 1,
        };
        let expected = crate::reassemble::reassemble_subband_5x3(&band, &[cb], 31, 0.5).unwrap();
        let mut surface = CoefficientSurface::<i32>::new(&levels).unwrap();
        let allocation = surface.bands[0][0].data.as_ptr();
        surface.scatter_5x3(0, 0, cb, 31, 0.5).unwrap();
        // The sink does not retain the borrow; reset before synthesis.
        block.reset_for_decode(SubBandOrientation::HH, 1, 1);
        let grid = surface.into_idwt_5x3(1).unwrap();
        assert_eq!(grid.data, expected);
        assert_eq!(grid.data.as_ptr(), allocation);
        assert_eq!(grid.data[0], i32::MAX);
        assert_eq!(grid.data[1], i32::MIN);
    }

    #[test]
    fn synthesis_moves_ll_and_releases_consumed_bands_before_next_level() {
        use std::cell::Cell;

        // Logical live values provide a deterministic lifetime check without
        // instrumenting a system allocator or depending on RSS behaviour.
        thread_local! {
            static LIVE: Cell<usize> = const { Cell::new(0) };
            static CALLS: Cell<usize> = const { Cell::new(0) };
        }
        struct Tracked {
            _value: u8,
        }
        impl Default for Tracked {
            fn default() -> Self {
                LIVE.with(|live| live.set(live.get() + 1));
                Self { _value: 0 }
            }
        }
        impl Clone for Tracked {
            fn clone(&self) -> Self {
                Self::default()
            }
        }
        impl Drop for Tracked {
            fn drop(&mut self) {
                LIVE.with(|live| live.set(live.get() - 1));
            }
        }
        // Mirrors the existing ten-argument inverse-DWT interface; this
        // callback verifies ownership lifetimes rather than wavelet math.
        #[allow(clippy::too_many_arguments)]
        fn inverse(
            _ll: &[Tracked],
            ll_dims: (usize, usize),
            _hl: &[Tracked],
            hl_dims: (usize, usize),
            _lh: &[Tracked],
            lh_dims: (usize, usize),
            _hh: &[Tracked],
            _hh_dims: (usize, usize),
            _i0: i32,
            _j0: i32,
            _threads: usize,
        ) -> Result<Interleaved2D<Tracked>, Error> {
            CALLS.with(|calls| calls.set(calls.get() + 1));
            // At the first step: four LL + twelve high-band + forty-eight
            // future-band values. At the second: sixteen carry + forty-eight
            // high-band values. A cloned LL or retained consumed bands would
            // increase this count and fail before the next output allocates.
            LIVE.with(|live| assert_eq!(live.get(), 64));
            let width = ll_dims.0 + hl_dims.0;
            let height = ll_dims.1 + lh_dims.1;
            Ok(Interleaved2D {
                data: vec![Tracked::default(); width * height],
                width,
                height,
            })
        }
        let levels = derive_resolution_levels(
            TileComponentGeometry {
                tcx0: 0,
                tcy0: 0,
                tcx1: 8,
                tcy1: 8,
            },
            2,
        );
        let surface = CoefficientSurface::<Tracked>::new(&levels).unwrap();
        LIVE.with(|live| assert_eq!(live.get(), 64));
        let grid = surface.synthesize(inverse, 1).unwrap();
        assert_eq!((grid.width, grid.height), (8, 8));
        CALLS.with(|calls| assert_eq!(calls.get(), 2));
        LIVE.with(|live| assert_eq!(live.get(), 64));
        drop(grid);
        LIVE.with(|live| assert_eq!(live.get(), 0));
    }

    #[test]
    fn surface_rejects_invalid_layout_before_allocating() {
        assert!(matches!(
            CoefficientSurface::<i32>::new(&[]),
            Err(Error::InvalidMarkerLength)
        ));
        let mut levels = one_level();
        levels[0].sub_bands[0].orientation = SubBandOrientation::HL;
        assert!(matches!(
            CoefficientSurface::<i32>::new(&levels),
            Err(Error::InvalidMarkerLength)
        ));
        levels[0].sub_bands.clear();
        assert!(matches!(
            CoefficientSurface::<f64>::new(&levels),
            Err(Error::InvalidMarkerLength)
        ));
    }

    #[test]
    fn later_invalid_layout_constructs_no_coefficients() {
        use std::cell::Cell;

        thread_local! {
            static CONSTRUCTIONS: Cell<usize> = const { Cell::new(0) };
        }
        #[derive(Clone)]
        struct ConstructionWitness {
            _value: u8,
        }
        impl Default for ConstructionWitness {
            fn default() -> Self {
                CONSTRUCTIONS.with(|count| count.set(count.get() + 1));
                Self { _value: 0 }
            }
        }

        let levels = derive_resolution_levels(
            TileComponentGeometry {
                tcx0: 0,
                tcy0: 0,
                tcx1: 8,
                tcy1: 8,
            },
            2,
        );
        for invalid_kind in 0..4 {
            let mut invalid = levels.clone();
            match invalid_kind {
                0 => invalid[1].r = 2,
                1 => invalid[1].sub_bands.clear(),
                2 => invalid[1].sub_bands[0].orientation = SubBandOrientation::LL,
                _ => invalid[2].sub_bands[2].orientation = SubBandOrientation::HL,
            }
            CONSTRUCTIONS.with(|count| count.set(0));
            assert!(matches!(
                CoefficientSurface::<ConstructionWitness>::new(&invalid),
                Err(Error::InvalidMarkerLength)
            ));
            // Live-value counts alone would miss an early allocation that
            // gets dropped while returning the later validation error.
            CONSTRUCTIONS.with(|count| assert_eq!(count.get(), 0));
        }
    }
}
