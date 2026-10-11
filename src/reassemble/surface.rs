//! Component-local coefficient storage for immediate block reconstruction.
//!
//! Only geometry and reconstructed coefficients survive a scatter. The
//! caller can reset or drop the code-block immediately afterwards. This
//! module uses the existing OxideAV dequantisation and inverse DWT routines;
//! it changes the lifetime of block state, not their arithmetic.

use super::{check_placement, r_qb_to_i32, CodedCodeBlock, SubBandQuantization};
use crate::dequant::{
    irreversible_step_size, qb_signed, reconstruct_irreversible, reconstruct_reversible,
};
use crate::dwt::{sr_2d_5x3, sr_2d_9x7, Interleaved2D};
use crate::geometry::{ResolutionLevel, SubBand, SubBandOrientation};
use crate::Error;

#[derive(Clone, Copy)]
struct BandRegion {
    band: SubBand,
    offset: usize,
    len: usize,
}

/// One allocation contains all coded bands of the kept resolution levels.
/// Uncoded positions stay zero. The overlap guard costs one bit per sample
/// and is released before synthesis; no decoded block is retained here.
pub(crate) struct CoefficientSurface<'a, T> {
    levels: &'a [ResolutionLevel],
    bands: Vec<Vec<BandRegion>>,
    data: Vec<T>,
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
) -> Result<Interleaved2D<T>, Error>;

impl<'a, T: Default + Clone> CoefficientSurface<'a, T> {
    pub(crate) fn new(levels: &'a [ResolutionLevel]) -> Result<Self, Error> {
        if levels.is_empty() {
            return Err(Error::InvalidMarkerLength);
        }
        let mut count = 0usize;
        let mut bands = Vec::with_capacity(levels.len());
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
                regions.push(BandRegion {
                    band,
                    offset: count,
                    len,
                });
                count = count.checked_add(len).ok_or(Error::InvalidMarkerLength)?;
            }
            bands.push(regions);
        }
        Ok(Self {
            levels,
            bands,
            data: vec![T::default(); count],
            written: vec![0; count.div_ceil(64)],
        })
    }

    fn scatter_with<F: Fn(u32, i64) -> T>(
        &mut self,
        resolution: usize,
        sub_band: usize,
        cb: CodedCodeBlock<'_>,
        reconstruct: F,
    ) -> Result<(), Error> {
        let region = *self
            .bands
            .get(resolution)
            .and_then(|bands| bands.get(sub_band))
            .ok_or(Error::InvalidPacketHeader)?;
        check_placement(&region.band, &cb.placement, cb.coefficients)?;
        let width = region.band.width() as usize;
        let dx = (cb.placement.x0 - region.band.tbx0) as usize;
        let dy = (cb.placement.y0 - region.band.tby0) as usize;
        let effective_nb = cb.coefficients.effective_nb_resolver(cb.nb);
        for v in 0..cb.coefficients.height() {
            for u in 0..cb.coefficients.width() {
                let target = region.offset + (dy + v) * width + dx + u;
                let mask = 1u64 << (target % 64);
                let word = &mut self.written[target / 64];
                if *word & mask != 0 {
                    return Err(Error::InvalidMarkerLength);
                }
                *word |= mask;
                let qb = qb_signed(cb.coefficients.coefficient(u, v));
                let nb = effective_nb(u, v);
                self.data[target] = reconstruct(nb, qb);
            }
        }
        Ok(())
    }

    fn synthesize(self, inverse: Inverse2D<T>) -> Result<Interleaved2D<T>, Error> {
        let Self {
            levels,
            bands,
            data,
            written,
        } = self;
        // No more scatters are possible once synthesis owns the surface.
        drop(written);
        let ll = bands[0][0];
        let mut carry_dims = (ll.band.width() as usize, ll.band.height() as usize);
        if levels.len() == 1 {
            return Ok(Interleaved2D {
                data,
                width: carry_dims.0,
                height: carry_dims.1,
            });
        }
        let mut carry = data[ll.offset..ll.offset + ll.len].to_vec();
        for (level, regions) in levels.iter().zip(&bands).skip(1) {
            let hl = regions[0];
            let lh = regions[1];
            let hh = regions[2];
            // These are exactly the band slices and origin casts passed by
            // the existing public IDWT cascade. No precision or rounding
            // change is introduced by the backing surface.
            let grid = inverse(
                &carry,
                carry_dims,
                &data[hl.offset..hl.offset + hl.len],
                (hl.band.width() as usize, hl.band.height() as usize),
                &data[lh.offset..lh.offset + lh.len],
                (lh.band.width() as usize, lh.band.height() as usize),
                &data[hh.offset..hh.offset + hh.len],
                (hh.band.width() as usize, hh.band.height() as usize),
                level.trx0 as i32,
                level.try0 as i32,
            )?;
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

impl CoefficientSurface<'_, i32> {
    pub(crate) fn scatter_5x3(
        &mut self,
        resolution: usize,
        sub_band: usize,
        cb: CodedCodeBlock<'_>,
        mb: u32,
        r: f64,
    ) -> Result<(), Error> {
        self.scatter_with(resolution, sub_band, cb, |nb, qb| {
            r_qb_to_i32(reconstruct_reversible(qb, mb, nb, r))
        })
    }

    pub(crate) fn into_idwt_5x3(self) -> Result<Interleaved2D<i32>, Error> {
        self.synthesize(sr_2d_5x3)
    }
}

impl CoefficientSurface<'_, f64> {
    pub(crate) fn scatter_9x7(
        &mut self,
        resolution: usize,
        sub_band: usize,
        cb: CodedCodeBlock<'_>,
        quant: SubBandQuantization,
        r: f64,
    ) -> Result<(), Error> {
        let step = irreversible_step_size(quant.rb, quant.step);
        self.scatter_with(resolution, sub_band, cb, |nb, qb| {
            reconstruct_irreversible(qb, quant.mb, nb, step, r)
        })
    }

    pub(crate) fn into_idwt_9x7(self) -> Result<Interleaved2D<f64>, Error> {
        self.synthesize(sr_2d_9x7)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dequant::StepSize;
    use crate::geometry::{derive_resolution_levels, PrecinctCodeBlock, TileComponentGeometry};
    use crate::reassemble::{idwt_5x3, idwt_9x7, BlockSource};
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
                if (x - band.tbx0 + 2 * (y - band.tby0) + mode) % 5 == 0 {
                    continue;
                }
                let placement = PrecinctCodeBlock {
                    cbx: (x - band.tbx0) / 3,
                    cby: (y - band.tby0) / 2,
                    x0: x,
                    y0: y,
                    x1: (x + 3).min(band.tbx1),
                    y1: (y + 2).min(band.tby1),
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
        for (x0, y0) in [(0, 0), (3, 1), (0x8000_0003, 0x8000_0001)] {
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
                            let new = reversible.into_idwt_5x3().unwrap();
                            assert_eq!((new.width, new.height), (old.width, old.height));
                            assert_eq!(new.data, old.data);
                            let old = idwt_9x7(levels, &source, &quant_per_level, 0.5).unwrap();
                            let new = irreversible.into_idwt_9x7().unwrap();
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
        let grid = surface.into_idwt_5x3().unwrap();
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
        let allocation = surface.data.as_ptr();
        surface.scatter_5x3(0, 0, cb, 31, 0.5).unwrap();
        // The sink does not retain the borrow; reset before synthesis.
        block.reset_for_decode(SubBandOrientation::HH, 1, 1);
        let grid = surface.into_idwt_5x3().unwrap();
        assert_eq!(grid.data, expected);
        assert_eq!(grid.data.as_ptr(), allocation);
        assert_eq!(grid.data[0], i32::MAX);
        assert_eq!(grid.data[1], i32::MIN);
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
}
