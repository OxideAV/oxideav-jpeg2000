//! Tile encoder: forward DWT → tier-1 EBCOT → tier-2 packet layout.
//!
//! The code mirrors [`crate::decode::tile`] on the output side. We
//! build the same per-resolution / per-subband layouts, run the
//! forward 5/3 lifting (reversible integer) or 9/7 lifting
//! (irreversible float + scalar quantisation), encode each code-block
//! with [`super::t1::encode_cblk`], then emit one packet per precinct
//! per resolution per layer (LRCP order, single layer).

use super::dwt::{fdwt_53, fdwt_97};
use super::t1::{encode_cblk, EncodedCblk};
use crate::decode::tile::{build_subbands, SubbandInfo};
use oxideav_core::{Error, Result};

/// Wavelet transform kind selected by the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransformKind {
    /// 5/3 reversible integer (Part-1 lossless default).
    Reversible53,
    /// 9/7 irreversible float with per-band scalar quantisation
    /// (Part-1 lossy default).
    Irreversible97,
}

/// Encoder-side equivalent of the decoder's `PrecinctState`. We keep
/// one of these per sub-band (single precinct per sub-band at PPx =
/// PPy = 15).
struct EncPrecinct {
    cblks_w: usize,
    cblks_h: usize,
    /// Per code-block encode results.
    cblks: Vec<EncodedCblk>,
    /// `oneplushalf`-style "inclusion" flag: true if the block has any
    /// passes carried in this packet.
    included: Vec<bool>,
}

struct EncResolution {
    /// Layout of the three sub-bands at this resolution (HL/LH/HH) or,
    /// for resolution 0, the single LL band.
    subbands: Vec<SubbandInfo>,
    /// One precinct per sub-band.
    precincts: Vec<EncPrecinct>,
}

/// Output of `encode_tile`: the concatenated packet body bytes (what
/// goes between SOD and the next SOT / EOC).
pub struct EncodedTile {
    pub body: Vec<u8>,
}

/// Encode a single tile.
///
/// - `comp_planes`: per-component sample arrays, already DC-level
///   shifted (for unsigned streams the caller must subtract
///   `1 << (prec - 1)`).
/// - `comp_sizes`: `(x0, y0, x1, y1)` rectangles for each component.
/// - `num_decomp`: number of DWT decomposition levels (5/3 default 5).
/// - `cblk_w_log2`, `cblk_h_log2`: code-block dimensions in log2.
/// - `guard_bits`: guard bits from QCD (2 is the default).
#[allow(clippy::too_many_arguments)]
pub fn encode_tile(
    comp_planes: &[Vec<i32>],
    comp_sizes: &[(u32, u32, u32, u32)],
    num_decomp: u8,
    cblk_w_log2: u8,
    cblk_h_log2: u8,
    guard_bits: u8,
    precision: u32,
) -> Result<EncodedTile> {
    if comp_planes.len() != comp_sizes.len() {
        return Err(Error::invalid(
            "jpeg2000: component plane / size array length mismatch",
        ));
    }
    let num_comps = comp_planes.len();
    let num_res = (num_decomp as usize) + 1;

    // For each component, build resolution → sub-band layouts, run the
    // forward DWT, split into code-blocks, run tier-1.
    let mut per_comp: Vec<Vec<EncResolution>> = Vec::with_capacity(num_comps);
    for (comp_idx, &(cx0, cy0, cx1, cy1)) in comp_sizes.iter().enumerate() {
        let comp_w = (cx1 - cx0) as usize;
        let comp_h = (cy1 - cy0) as usize;
        let subbands_all = build_subbands(cx0, cy0, cx1, cy1, num_decomp);

        // Run a quadrant-first forward DWT pyramid on a working copy.
        let mut canvas = comp_planes[comp_idx].clone();
        if canvas.len() != comp_w * comp_h {
            return Err(Error::invalid(
                "jpeg2000: component plane size mismatch with comp_sizes",
            ));
        }
        // Apply `num_decomp` levels of forward DWT. At each level, the
        // low-pass quadrant of the previous canvas becomes the input
        // to the next level. Strides don't change — we work in place.
        let mut cur_w = comp_w;
        let mut cur_h = comp_h;
        for _level in 0..num_decomp as usize {
            if cur_w < 2 || cur_h < 2 {
                break;
            }
            fdwt_53(&mut canvas, cur_w, cur_h, comp_w);
            // LL now occupies the top-left ceil(w/2) × ceil(h/2)
            // quadrant. For the next level we operate on that
            // quadrant only, but we need to "compact" it into a
            // sub-canvas because `fdwt_53` assumes the full rectangle
            // is the input. Copy into a smaller working canvas:
            let sub_w = cur_w.div_ceil(2);
            let sub_h = cur_h.div_ceil(2);
            let mut sub = vec![0i32; sub_w * sub_h];
            for y in 0..sub_h {
                for x in 0..sub_w {
                    sub[y * sub_w + x] = canvas[y * comp_w + x];
                }
            }
            // Run the next level on the sub-canvas.
            // NOTE: we defer the actual call — at the top of the next
            // iteration we need `canvas` to host the sub-plane so the
            // call matches. Instead, splat the sub-canvas back in.
            for y in 0..sub_h {
                for x in 0..sub_w {
                    canvas[y * comp_w + x] = sub[y * sub_w + x];
                }
            }
            cur_w = sub_w;
            cur_h = sub_h;
        }

        // Canvas now holds the fully transformed pyramid in the
        // "quadrant-packed" layout:
        //   LL_0 at (0..=ll_w, 0..=ll_h)
        //   HL_1 at (ll_w.., 0..ll_h)
        //   LH_1 at (0..ll_w, ll_h..)
        //   HH_1 at (ll_w.., ll_h..)
        //   (and similarly recursively inside the LL_0 quadrant at
        //    higher levels)
        //
        // Build resolution structures: for each resolution + sub-band,
        // extract the corresponding rectangle into a standalone buffer
        // and code it.
        let mut res_out: Vec<EncResolution> = Vec::with_capacity(num_res);
        for resno in 0..num_res as u8 {
            let subs: Vec<SubbandInfo> = subbands_all
                .iter()
                .copied()
                .filter(|sb| sb.resno == resno)
                .collect();
            let mut precincts = Vec::with_capacity(subs.len());
            for sb in &subs {
                let bw = (sb.x1 - sb.x0) as usize;
                let bh = (sb.y1 - sb.y0) as usize;
                if bw == 0 || bh == 0 {
                    precincts.push(EncPrecinct {
                        cblks_w: 1,
                        cblks_h: 1,
                        cblks: Vec::new(),
                        included: Vec::new(),
                    });
                    continue;
                }
                // Copy the sub-band out of the canvas into a compact
                // buffer. Mapping from `sb.x0..x1` (in component coords
                // at the band's subsampled grid) back into canvas
                // coordinates: the canvas is at the component's grid
                // (full-resolution), but the samples live in the
                // pyramid's quadrant-packed positions. We need to know
                // where each sub-band lies in `canvas` coords.
                //
                // With our level-by-level DWT that compacts LL into the
                // top-left quadrant before running the next level, the
                // final canvas holds:
                //   resolution r, band HL/LH/HH occupies a specific
                //   quadrant at the appropriate scale.
                //
                // We derive the canvas rectangle from the sub-band's
                // `(x0, y0, x1, y1)` — at resolution r those
                // coordinates are in the r-th subsampled grid. On our
                // canvas (full-resolution), the sub-band occupies the
                // same numeric range but shifted into the relevant
                // quadrant.
                //
                // Concretely: at resolution r, the canvas quadrant for
                // LL_r is `[0..w_r] × [0..h_r]` where `w_r = ceil(W/2^(L-r+1))`
                // times something... ugh. Let me use the simpler
                // scheme: we know the final canvas size equals the
                // component. We track per-level (w_r, h_r) = canvas
                // dimensions after r levels.
                let level_from_top = num_decomp as usize - resno as usize;
                // After `level_from_top` levels of forward DWT, canvas
                // at scale r has width ceil(W / 2^level_from_top) and
                // similarly h.
                let mut scale_w = comp_w;
                let mut scale_h = comp_h;
                for _ in 0..level_from_top {
                    scale_w = scale_w.div_ceil(2);
                    scale_h = scale_h.div_ceil(2);
                }
                // sub-band position within that scale canvas.
                let (band_cx0, band_cy0) = match sb.band_kind {
                    0 => (0usize, 0usize),                           // LL
                    1 => (scale_w.div_ceil(2), 0),                   // HL: right half, top
                    2 => (0, scale_h.div_ceil(2)),                   // LH: left half, bottom
                    3 => (scale_w.div_ceil(2), scale_h.div_ceil(2)), // HH
                    _ => (0, 0),
                };
                let mut band_buf = vec![0i32; bw * bh];
                for by in 0..bh {
                    for bx in 0..bw {
                        band_buf[by * bw + bx] = canvas[(band_cy0 + by) * comp_w + (band_cx0 + bx)];
                    }
                }

                // Tier-1 on every code-block.
                let cw = 1u32 << cblk_w_log2;
                let ch = 1u32 << cblk_h_log2;
                let cblks_w = (bw as u32).div_ceil(cw) as usize;
                let cblks_h = (bh as u32).div_ceil(ch) as usize;
                let mut cblks: Vec<EncodedCblk> = Vec::with_capacity(cblks_w * cblks_h);
                let mut included = Vec::with_capacity(cblks_w * cblks_h);
                // Band numbps = guard_bits + eps - 1. For reversible
                // 5/3, eps is determined by the image precision +
                // log2_gain_b. Use the natural reversible choice:
                // eps_b = precision + log2_gain_b. guard_bits = 2.
                let log2_gain: i32 = match sb.band_kind {
                    0 => 0,
                    1 | 2 => 1,
                    3 => 2,
                    _ => 0,
                };
                let eps = precision as i32 + log2_gain;
                let band_numbps = guard_bits as i32 + eps - 1;
                for cy in 0..cblks_h {
                    for cx in 0..cblks_w {
                        let x0 = sb.x0 + cx as u32 * cw;
                        let y0 = sb.y0 + cy as u32 * ch;
                        let x1 = (x0 + cw).min(sb.x1);
                        let y1 = (y0 + ch).min(sb.y1);
                        let cw_real = (x1 - x0) as usize;
                        let ch_real = (y1 - y0) as usize;
                        let rel_x = (x0 - sb.x0) as usize;
                        let rel_y = (y0 - sb.y0) as usize;
                        let mut local = vec![0i32; cw_real * ch_real];
                        for ly in 0..ch_real {
                            for lx in 0..cw_real {
                                local[ly * cw_real + lx] =
                                    band_buf[(rel_y + ly) * bw + (rel_x + lx)];
                            }
                        }
                        let enc = encode_cblk(&local, cw_real, ch_real, band_numbps, sb.orient);
                        // "Included" iff the block carries any
                        // non-trivial passes — i.e. at least one
                        // non-zero sample exists. We include the block
                        // whenever its missing_msb is strictly less
                        // than Mb (= band_numbps + 1).
                        let mb = band_numbps + 1;
                        let block_included = (enc.missing_msb as i32) < mb;
                        included.push(block_included);
                        cblks.push(enc);
                    }
                }
                precincts.push(EncPrecinct {
                    cblks_w,
                    cblks_h,
                    cblks,
                    included,
                });
            }
            res_out.push(EncResolution {
                subbands: subs,
                precincts,
            });
        }
        per_comp.push(res_out);
    }

    // Tier-2: emit packets in LRCP order (single layer).
    let mut body: Vec<u8> = Vec::new();
    for resno in 0..num_res {
        for per_comp_entry in per_comp.iter_mut().take(num_comps) {
            emit_packet(&mut body, &mut per_comp_entry[resno])?;
        }
    }

    Ok(EncodedTile { body })
}

/// Encode a single tile using the 9/7 irreversible transform with
/// per-band scalar quantisation.
///
/// - `comp_planes_f32`: per-component sample arrays in `f32`, already
///   DC-level shifted.
/// - `band_stepsizes`: pre-computed quantisation step sizes indexed by
///   the canonical band order (LL of resolution 0, then HL/LH/HH per
///   resolution). Must have `3 * num_decomp + 1` entries.
/// - `band_eps`: matching `eps_b` values for the QCD. Same ordering.
#[allow(clippy::too_many_arguments)]
pub fn encode_tile_97(
    comp_planes_f32: &[Vec<f32>],
    comp_sizes: &[(u32, u32, u32, u32)],
    num_decomp: u8,
    cblk_w_log2: u8,
    cblk_h_log2: u8,
    guard_bits: u8,
    band_stepsizes: &[f32],
    band_eps: &[u8],
) -> Result<EncodedTile> {
    if comp_planes_f32.len() != comp_sizes.len() {
        return Err(Error::invalid(
            "jpeg2000: component plane / size array length mismatch",
        ));
    }
    let num_bands = 3 * (num_decomp as usize) + 1;
    if band_stepsizes.len() != num_bands || band_eps.len() != num_bands {
        return Err(Error::invalid(
            "jpeg2000: band stepsize / eps length mismatch",
        ));
    }
    let num_comps = comp_planes_f32.len();
    let num_res = (num_decomp as usize) + 1;

    let mut per_comp: Vec<Vec<EncResolution>> = Vec::with_capacity(num_comps);
    for (comp_idx, &(cx0, cy0, cx1, cy1)) in comp_sizes.iter().enumerate() {
        let comp_w = (cx1 - cx0) as usize;
        let comp_h = (cy1 - cy0) as usize;
        let subbands_all = build_subbands(cx0, cy0, cx1, cy1, num_decomp);

        let mut canvas = comp_planes_f32[comp_idx].clone();
        if canvas.len() != comp_w * comp_h {
            return Err(Error::invalid(
                "jpeg2000: component plane size mismatch with comp_sizes",
            ));
        }

        // Level-by-level forward 9/7 pyramid. Same driver shape as the
        // 5/3 path: at each level, transform the current active
        // rectangle, then compact the low-pass quadrant so the next
        // level operates on a smaller top-left region.
        let mut cur_w = comp_w;
        let mut cur_h = comp_h;
        for _level in 0..num_decomp as usize {
            if cur_w < 2 || cur_h < 2 {
                break;
            }
            fdwt_97(&mut canvas, cur_w, cur_h, comp_w);
            let sub_w = cur_w.div_ceil(2);
            let sub_h = cur_h.div_ceil(2);
            let mut sub = vec![0f32; sub_w * sub_h];
            for y in 0..sub_h {
                for x in 0..sub_w {
                    sub[y * sub_w + x] = canvas[y * comp_w + x];
                }
            }
            for y in 0..sub_h {
                for x in 0..sub_w {
                    canvas[y * comp_w + x] = sub[y * sub_w + x];
                }
            }
            cur_w = sub_w;
            cur_h = sub_h;
        }

        // Build per-resolution / per-subband tier-1 output.
        let mut res_out: Vec<EncResolution> = Vec::with_capacity(num_res);
        for resno in 0..num_res as u8 {
            let subs: Vec<SubbandInfo> = subbands_all
                .iter()
                .copied()
                .filter(|sb| sb.resno == resno)
                .collect();
            let mut precincts = Vec::with_capacity(subs.len());
            for sb in &subs {
                let bw = (sb.x1 - sb.x0) as usize;
                let bh = (sb.y1 - sb.y0) as usize;
                if bw == 0 || bh == 0 {
                    precincts.push(EncPrecinct {
                        cblks_w: 1,
                        cblks_h: 1,
                        cblks: Vec::new(),
                        included: Vec::new(),
                    });
                    continue;
                }
                let level_from_top = num_decomp as usize - resno as usize;
                let mut scale_w = comp_w;
                let mut scale_h = comp_h;
                for _ in 0..level_from_top {
                    scale_w = scale_w.div_ceil(2);
                    scale_h = scale_h.div_ceil(2);
                }
                let (band_cx0, band_cy0) = match sb.band_kind {
                    0 => (0usize, 0usize),
                    1 => (scale_w.div_ceil(2), 0),
                    2 => (0, scale_h.div_ceil(2)),
                    3 => (scale_w.div_ceil(2), scale_h.div_ceil(2)),
                    _ => (0, 0),
                };

                // Quantise: q = sign(c) * floor(|c| / stepsize * 2) / 2
                // The *2 factor here comes from T.800 Eq E-6's
                // `2 * sign(y) * floor(|y| / 2Δ)` convention — but
                // OpenJPEG folds the factor-of-2 into the decoder by
                // using `scale = 0.5 * stepsize` and having tier-1 emit
                // `2|q| | 1`. So on the encoder side we produce
                // `q = sign(c) * floor(|c| / stepsize)` and let tier-1
                // multiply by 2 internally.
                let stepsize = band_stepsizes[sb.band_idx];
                if !stepsize.is_finite() || stepsize <= 0.0 {
                    return Err(Error::invalid("jpeg2000: invalid 9/7 stepsize"));
                }
                let mut band_buf_i = vec![0i32; bw * bh];
                for by in 0..bh {
                    for bx in 0..bw {
                        let c = canvas[(band_cy0 + by) * comp_w + (band_cx0 + bx)];
                        // Plain dead-zone scalar quantiser.
                        let qf = c / stepsize;
                        let q = if qf >= 0.0 {
                            qf.floor() as i32
                        } else {
                            -((-qf).floor() as i32)
                        };
                        band_buf_i[by * bw + bx] = q;
                    }
                }

                // Tier-1 on each code-block. Band numbps follows the
                // decoder's formula: `band_numbps = guard_bits + eps - 1`.
                let cw = 1u32 << cblk_w_log2;
                let ch = 1u32 << cblk_h_log2;
                let cblks_w = (bw as u32).div_ceil(cw) as usize;
                let cblks_h = (bh as u32).div_ceil(ch) as usize;
                let mut cblks: Vec<EncodedCblk> = Vec::with_capacity(cblks_w * cblks_h);
                let mut included = Vec::with_capacity(cblks_w * cblks_h);
                let eps = band_eps[sb.band_idx] as i32;
                let band_numbps = guard_bits as i32 + eps - 1;
                for cy in 0..cblks_h {
                    for cx in 0..cblks_w {
                        let x0 = sb.x0 + cx as u32 * cw;
                        let y0 = sb.y0 + cy as u32 * ch;
                        let x1 = (x0 + cw).min(sb.x1);
                        let y1 = (y0 + ch).min(sb.y1);
                        let cw_real = (x1 - x0) as usize;
                        let ch_real = (y1 - y0) as usize;
                        let rel_x = (x0 - sb.x0) as usize;
                        let rel_y = (y0 - sb.y0) as usize;
                        let mut local = vec![0i32; cw_real * ch_real];
                        for ly in 0..ch_real {
                            for lx in 0..cw_real {
                                local[ly * cw_real + lx] =
                                    band_buf_i[(rel_y + ly) * bw + (rel_x + lx)];
                            }
                        }
                        let enc = encode_cblk(&local, cw_real, ch_real, band_numbps, sb.orient);
                        let mb = band_numbps + 1;
                        let block_included = (enc.missing_msb as i32) < mb;
                        included.push(block_included);
                        cblks.push(enc);
                    }
                }
                precincts.push(EncPrecinct {
                    cblks_w,
                    cblks_h,
                    cblks,
                    included,
                });
            }
            res_out.push(EncResolution {
                subbands: subs,
                precincts,
            });
        }
        per_comp.push(res_out);
    }

    // Tier-2.
    let mut body: Vec<u8> = Vec::new();
    for resno in 0..num_res {
        for per_comp_entry in per_comp.iter_mut().take(num_comps) {
            emit_packet(&mut body, &mut per_comp_entry[resno])?;
        }
    }
    Ok(EncodedTile { body })
}

/// Emit one packet (LRCP, single layer, single precinct-per-band).
///
/// Packet layout (T.800 §B.9):
/// - Packet header: bit-packed inclusion + Lblock + length fields.
/// - Packet body: concatenated code-block compressed data.
fn emit_packet(out: &mut Vec<u8>, res: &mut EncResolution) -> Result<()> {
    // Bit-I/O writer mirroring the decoder's `Bio`.
    let mut bio = BioWriter::new();

    // Zero-or-more included cblks across all sub-bands of this
    // resolution → set the "packet has data" flag.
    let any_included = res.precincts.iter().any(|p| p.included.iter().any(|&b| b));
    if !any_included {
        bio.write_bit(0);
        bio.flush_to(out);
        return Ok(());
    }

    bio.write_bit(1);

    // Per sub-band: inclusion + zero-bitplane tag trees + num-passes +
    // Lblock growth + length.
    for (sb_idx, sb) in res.subbands.iter().enumerate() {
        let p = &res.precincts[sb_idx];
        if p.cblks.is_empty() {
            continue;
        }
        // Build inclusion tag tree leaves: 0 = included here, big value
        // otherwise. Since this is layer 0 we always pass threshold 1,
        // so the leaf must be 0 for included blocks.
        let w = p.cblks_w;
        let h = p.cblks_h;
        let mut incl_leaves = vec![u32::MAX; w * h];
        let mut zb_leaves = vec![0u32; w * h];
        for i in 0..w * h {
            if p.included[i] {
                incl_leaves[i] = 0;
                zb_leaves[i] = p.cblks[i].missing_msb;
            }
        }
        encode_tagtree(&mut bio, w, h, &incl_leaves, 1);
        // zero-bitplane tag trees: encoded per block if included. For
        // each included block, we iterate thresholds from 1 upward
        // until the leaf value < threshold. OpenJPEG instead writes
        // thresholds: we pass incl-ordered threshold `missing_msb + 1`.
        for cy in 0..h {
            for cx in 0..w {
                let idx = cy * w + cx;
                if !p.included[idx] {
                    continue;
                }
                // zero-bitplane value = missing_msb. Encode threshold
                // sweep from 1 upward — this emits `missing_msb` zero
                // bits and then a single one bit at threshold
                // `missing_msb + 1`.
                let mm = zb_leaves[idx];
                let mut tree = OneLeafTree::new(mm);
                for th in 1..=mm + 1 {
                    tree.decode_or_encode(&mut bio, th);
                }
                let _ = sb;
            }
        }
        // For each included block: num_passes + lblock growth + length.
        for (idx, cblk) in p.cblks.iter().enumerate() {
            if !p.included[idx] {
                continue;
            }
            write_num_passes(&mut bio, cblk.total_passes);
            // Adaptive Lblock. We start Lblock at 3 (matches the
            // decoder's `CblkState::default`). For a single-layer
            // stream we don't need to grow it past what's required.
            let mut lblock = 3u32;
            loop {
                let (needs_growth, _) =
                    bits_needed(cblk.data.len() as u32, cblk.total_passes, lblock);
                if !needs_growth {
                    break;
                }
                bio.write_bit(1);
                lblock += 1;
            }
            bio.write_bit(0);
            let total_len_bits = lblock + ilog2(cblk.total_passes);
            bio.write(total_len_bits, cblk.data.len() as u32);
        }
    }
    bio.inalign();
    bio.flush_to(out);
    // Append cblk data bytes after the header.
    for p in &res.precincts {
        for (idx, cblk) in p.cblks.iter().enumerate() {
            if !p.included[idx] {
                continue;
            }
            out.extend_from_slice(&cblk.data);
        }
    }
    Ok(())
}

/// Returns `(needs_growth, bits_used)`: whether the current Lblock is
/// insufficient to encode the block length.
fn bits_needed(length: u32, num_passes: u32, lblock: u32) -> (bool, u32) {
    let bits = lblock + ilog2(num_passes);
    if bits >= 32 {
        return (false, 31);
    }
    if length >= (1u32 << bits) {
        (true, bits)
    } else {
        (false, bits)
    }
}

fn ilog2(n: u32) -> u32 {
    if n == 0 {
        0
    } else {
        31 - n.leading_zeros()
    }
}

fn write_num_passes(bio: &mut BioWriter, n: u32) {
    // Inverse of `read_num_passes` in the decoder.
    if n == 1 {
        bio.write_bit(0);
        return;
    }
    bio.write_bit(1);
    if n == 2 {
        bio.write_bit(0);
        return;
    }
    bio.write_bit(1);
    if n <= 5 {
        // 2-bit field v < 3 ⇒ n = 3 + v
        bio.write(2, n - 3);
        return;
    }
    bio.write(2, 3);
    if n <= 36 {
        // 5-bit field v < 31 ⇒ n = 6 + v
        bio.write(5, n - 6);
        return;
    }
    bio.write(5, 31);
    // 7-bit field for the tail: n = 37 + v (0..=127)
    bio.write(7, n - 37);
}

/// Encode a tag tree so the decoder — given the sequence of per-leaf
/// threshold queries — recovers `values[]`.
///
/// We emit *value + 1* zero bits followed by one `1` bit at each
/// internal level to signal that the decoded lower bound has reached
/// the true value. This is the standard build per T.800 §B.10.2.
fn encode_tagtree(bio: &mut BioWriter, w: usize, h: usize, values: &[u32], threshold: u32) {
    // Reuse the simple-decoder structure. To stay consistent with the
    // decoder we emit threshold queries one by one: for each leaf, we
    // iterate the threshold 1..=threshold and emit the bit that the
    // decoder would expect.
    let mut tree = TagTreeEnc::new(w, h, values);
    for y in 0..h {
        for x in 0..w {
            for th in 1..=threshold {
                tree.emit(bio, x, y, th);
            }
        }
    }
}

struct TagTreeEnc {
    w: usize,
    h: usize,
    /// Running lower bound for each node, initialised to 0.
    low: Vec<Vec<u32>>,
    /// Per-level offsets.
    #[allow(dead_code)]
    level_dims: Vec<(usize, usize)>,
    /// Leaf values at level 0 (length `w * h`), min-combined as we go
    /// up the tree so interior nodes hold the minimum of their
    /// descendants.
    values: Vec<Vec<u32>>,
    /// True once we've emitted the terminating `1` for each node —
    /// mirrors the decoder's `value[idx] < u32::MAX` check.
    resolved: Vec<Vec<bool>>,
}

impl TagTreeEnc {
    fn new(w: usize, h: usize, values: &[u32]) -> Self {
        let mut levels = vec![(w, h)];
        let (mut lw, mut lh) = (w, h);
        while lw > 1 || lh > 1 {
            lw = lw.div_ceil(2);
            lh = lh.div_ceil(2);
            levels.push((lw, lh));
        }
        let mut vs: Vec<Vec<u32>> = Vec::with_capacity(levels.len());
        vs.push(values.to_vec());
        for i in 1..levels.len() {
            let (plw, plh) = levels[i - 1];
            let (clw, clh) = levels[i];
            let mut buf = vec![u32::MAX; clw * clh];
            for cy in 0..clh {
                for cx in 0..clw {
                    let mut m = u32::MAX;
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let px = cx * 2 + dx;
                            let py = cy * 2 + dy;
                            if px < plw && py < plh {
                                m = m.min(vs[i - 1][py * plw + px]);
                            }
                        }
                    }
                    buf[cy * clw + cx] = m;
                }
            }
            vs.push(buf);
        }
        let low = levels.iter().map(|&(lw, lh)| vec![0u32; lw * lh]).collect();
        let resolved = levels
            .iter()
            .map(|&(lw, lh)| vec![false; lw * lh])
            .collect();
        TagTreeEnc {
            w,
            h,
            low,
            level_dims: levels,
            values: vs,
            resolved,
        }
    }

    fn emit(&mut self, bio: &mut BioWriter, x: usize, y: usize, threshold: u32) {
        // Walk root → leaf, emitting bits so the decoder's step-by-step
        // procedure recovers the same `low` vs `value` comparisons.
        //
        // Decoder's inner loop (see `TagTree::decode`) is:
        //   while low < threshold && low < value[idx] {
        //       if bit == 1 { value[idx] = low; break; }
        //       else { low += 1; }
        //   }
        //
        // For each bit we emit:
        //   - '0' if `low + 1 <= node_value` (i.e. the true value is
        //     at least `low + 1` — decoder advances `low`).
        //   - '1' otherwise (= `low == node_value`, so the decoder
        //     concludes `value[idx] = low`).
        let nlvls = self.low.len();
        let mut stack: Vec<(usize, usize)> = Vec::with_capacity(nlvls);
        let mut cx = x;
        let mut cy = y;
        for lvl in 0..nlvls {
            stack.push((lvl, cx + cy * self.dim(lvl).0));
            cx /= 2;
            cy /= 2;
        }
        let mut low: u32 = 0;
        while let Some((lvl, idx_in_level)) = stack.pop() {
            if low > self.low[lvl][idx_in_level] {
                self.low[lvl][idx_in_level] = low;
            } else {
                low = self.low[lvl][idx_in_level];
            }
            let v = self.values[lvl][idx_in_level];
            while low < threshold && low < v {
                bio.write_bit(0);
                low += 1;
            }
            if low < threshold && low >= v && !self.resolved[lvl][idx_in_level] {
                // Signal the decoder that this node's value == low.
                // The decoder's `decode` loop condition `low < value`
                // fails after this bit causes `value[idx] = low`.
                bio.write_bit(1);
                self.resolved[lvl][idx_in_level] = true;
            }
            self.low[lvl][idx_in_level] = low;
        }
    }

    fn dim(&self, lvl: usize) -> (usize, usize) {
        if lvl >= self.low.len() {
            (0, 0)
        } else {
            let mut lw = self.w;
            let mut lh = self.h;
            for _ in 0..lvl {
                lw = lw.div_ceil(2);
                lh = lh.div_ceil(2);
            }
            (lw, lh)
        }
    }
}

/// Minimal single-leaf tag tree wrapper, used for zero-bitplane tag
/// trees where each cblk has its own tree of size 1×1.
struct OneLeafTree {
    value: u32,
    low: u32,
    resolved: bool,
}

impl OneLeafTree {
    fn new(value: u32) -> Self {
        OneLeafTree {
            value,
            low: 0,
            resolved: false,
        }
    }
    fn decode_or_encode(&mut self, bio: &mut BioWriter, threshold: u32) {
        while self.low < threshold && self.low < self.value {
            bio.write_bit(0);
            self.low += 1;
        }
        if self.low < threshold && !self.resolved {
            bio.write_bit(1);
            self.resolved = true;
        }
    }
}

/// Bit-packed writer mirroring the decoder's `Bio` — MSB-first with
/// the 0xFF stuff-bit rule (any byte that reaches 0xFF means the next
/// byte carries only 7 payload bits).
pub struct BioWriter {
    buf: Vec<u8>,
    /// Current byte under assembly.
    cur: u8,
    /// Remaining usable bits in `cur` (0..=8 when ff_pending is false;
    /// 0..=7 when ff_pending is true).
    ct: u32,
    /// True if the most recently flushed byte was 0xFF.
    ff_pending: bool,
}

impl Default for BioWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl BioWriter {
    pub fn new() -> Self {
        BioWriter {
            buf: Vec::new(),
            cur: 0,
            ct: 8,
            ff_pending: false,
        }
    }

    pub fn write_bit(&mut self, bit: u32) {
        if self.ct == 0 {
            // Emit the assembled byte and start a new one.
            self.flush_byte();
        }
        self.ct -= 1;
        if bit != 0 {
            self.cur |= 1u8 << self.ct;
        }
    }

    pub fn write(&mut self, n: u32, v: u32) {
        for i in (0..n).rev() {
            self.write_bit((v >> i) & 1);
        }
    }

    fn flush_byte(&mut self) {
        let b = self.cur;
        self.buf.push(b);
        self.cur = 0;
        if b == 0xFF {
            self.ff_pending = true;
            self.ct = 7;
        } else {
            self.ff_pending = false;
            self.ct = 8;
        }
    }

    /// Byte-align at end-of-header: pad the partially-filled byte with
    /// zeros and flush it.
    pub fn inalign(&mut self) {
        let filled = if self.ff_pending { 7 } else { 8 };
        if self.ct != filled {
            self.cur &= 0xFFu8.wrapping_shl(self.ct);
            self.flush_byte();
        }
    }

    /// Drain the buffered output into `dst`.
    pub fn flush_to(self, dst: &mut Vec<u8>) {
        let mut me = self;
        // If a partial byte is pending, flush it first.
        let filled = if me.ff_pending { 7 } else { 8 };
        if me.ct != filled {
            me.cur &= 0xFFu8.wrapping_shl(me.ct);
            me.flush_byte();
        }
        dst.extend_from_slice(&me.buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bio_writer_basic() {
        let mut bio = BioWriter::new();
        bio.write_bit(1);
        bio.write_bit(0);
        bio.write_bit(1);
        bio.write(5, 0b10110);
        let mut out = Vec::new();
        bio.flush_to(&mut out);
        assert_eq!(out, vec![0b1011_0110]);
    }

    #[test]
    fn bio_writer_ff_stuffing() {
        // Emit 8 one bits to produce 0xFF, then write more bits — the
        // next byte must carry only 7 usable bits.
        let mut bio = BioWriter::new();
        for _ in 0..8 {
            bio.write_bit(1);
        }
        // Next byte starts with a forced MSB = 0.
        bio.write_bit(0);
        bio.write_bit(1);
        let mut out = Vec::new();
        bio.flush_to(&mut out);
        assert_eq!(out[0], 0xFF);
        // The second byte has bit7 = 0 (forced), bit6 = 0 (our first
        // real bit was 0 ... wait, we wrote 0 then 1, so bit6 = 0,
        // bit5 = 1, lower padded to 0: 0010_0000 = 0x20).
        assert_eq!(out[1], 0b0010_0000);
    }
}
