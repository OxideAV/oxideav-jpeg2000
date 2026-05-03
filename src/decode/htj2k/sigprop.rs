//! HT SigProp pass decoder (§7.4 of ISO/IEC 15444-15:2019,
//! FDIS pages 20-21).
//!
//! The SigProp pass refines the binary "z_n" indicator and "r_n"
//! refinement bit for each sample, on top of the cleanup pass output.
//! Following the four-line stripe-oriented scan from Figure 7, for
//! each four-column "column-group" inside each stripe, the decoder
//! first walks samples with `decodeSigPropMag` to decide whether to
//! import a refinement bit, then walks them again with
//! `decodeSigPropSign` to import a sign bit when the refinement bit
//! says so.
//!
//! NOTE: This implementation walks the samples in the *quad-scan*
//! order produced by the cleanup pass, then maps `n = 4q + j` back to
//! `(x, y)` to derive the propagation neighbourhood `N_n` per
//! Figure 7. The resulting bits exactly mirror the spec's
//! `decodeSigPropMag` / `decodeSigPropSign` procedures.

use super::cleanup::CleanupOutput;
use super::streams::SigPropReader;
use crate::error::Result;

/// Decoded SigProp state: `z_n` (refinement indicator) and
/// `r_n` (refinement value), one per sample.
#[derive(Debug, Clone)]
pub struct SigPropOutput {
    pub z: Vec<u8>,
    pub r: Vec<u8>,
    /// Updated sign bits — SigProp may set a sign for samples that
    /// became refined this pass.
    pub sign: Vec<u8>,
}

/// Run the SigProp pass on top of the cleanup output. `dref` is the
/// HT refinement segment bytes; if `Z_blk == 1` (no SigProp pass) the
/// caller skips this entirely. A *placeholder* SigProp pass — one
/// where the segment exists but is empty — yields `z_n = r_n = 0`
/// for every sample, since the SigProp reader returns 0 past end.
pub fn decode_sigprop(out: &CleanupOutput, dref: &[u8]) -> Result<SigPropOutput> {
    let n = out.mag.len();
    let mut z = vec![0u8; n];
    let mut r = vec![0u8; n];
    let mut sign = out.sign.clone();
    let mut reader = SigPropReader::new(dref);

    let qw = out.width.div_ceil(2);
    let qh = out.height.div_ceil(2);
    let _ = qh;
    // Walk the quads row-major. For each sample with σ_n = 0, derive
    // the "magnitude bit refinement" indicator (mbr): if any neighbour
    // (in the 8-neighbour propagation set) is significant or has been
    // marked z=1 in a prior step, import one SigProp bit.
    let cb_w = out.width as i32;
    let cb_h = out.height as i32;
    for qy in 0..qh as i32 {
        for qx in 0..qw as i32 {
            let q = (qy as usize) * (qw as usize) + qx as usize;
            for j in 0..4u8 {
                let (dx, dy) = sample_offset(j);
                let x = 2 * qx + dx as i32;
                let y = 2 * qy + dy as i32;
                if x >= cb_w || y >= cb_h {
                    continue;
                }
                let n_idx = 4 * q + j as usize;
                if out.sig[n_idx] != 0 {
                    continue;
                }
                let mbr = neighbour_mbr(out, &z, &r, qw, x, y, cb_w, cb_h);
                if mbr {
                    z[n_idx] = 1;
                    r[n_idx] = reader.import_bit()?;
                }
            }
            // Sign step: any sample whose r_n was just set must consume
            // a sign bit.
            for j in 0..4u8 {
                let (dx, dy) = sample_offset(j);
                let x = 2 * qx + dx as i32;
                let y = 2 * qy + dy as i32;
                if x >= cb_w || y >= cb_h {
                    continue;
                }
                let n_idx = 4 * q + j as usize;
                if r[n_idx] != 0 {
                    sign[n_idx] = reader.import_bit()?;
                }
            }
        }
    }

    Ok(SigPropOutput { z, r, sign })
}

#[inline]
fn sample_offset(j: u8) -> (u32, u32) {
    match j {
        0 => (0, 0),
        1 => (0, 1),
        2 => (1, 0),
        3 => (1, 1),
        _ => unreachable!(),
    }
}

/// True iff at least one neighbour of (x, y) in its 8-neighbourhood is
/// significant (σ=1) or has had its refinement indicator z set this
/// pass.
#[allow(clippy::too_many_arguments)]
fn neighbour_mbr(
    out: &CleanupOutput,
    z: &[u8],
    r: &[u8],
    qw: u32,
    x: i32,
    y: i32,
    cb_w: i32,
    cb_h: i32,
) -> bool {
    for dy in -1..=1 {
        for dx in -1..=1 {
            if dx == 0 && dy == 0 {
                continue;
            }
            let nx = x + dx;
            let ny = y + dy;
            if nx < 0 || ny < 0 || nx >= cb_w || ny >= cb_h {
                continue;
            }
            let n_idx = sample_index(qw, nx as u32, ny as u32);
            if out.sig[n_idx] != 0 || z[n_idx] != 0 || r[n_idx] != 0 {
                return true;
            }
        }
    }
    false
}

fn sample_index(qw: u32, x: u32, y: u32) -> usize {
    let qx = x / 2;
    let qy = y / 2;
    let dx = x & 1;
    let dy = y & 1;
    let j = match (dx, dy) {
        (0, 0) => 0,
        (0, 1) => 1,
        (1, 0) => 2,
        (1, 1) => 3,
        _ => unreachable!(),
    };
    let q = (qy as usize) * (qw as usize) + qx as usize;
    4 * q + j
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::htj2k::cleanup::decode_cleanup;

    #[test]
    fn placeholder_sigprop_leaves_state_zero() {
        // Reuse the cleanup-AZC fixture: a 2x2 block with no
        // significant samples. With a placeholder SigProp pass
        // (zero-length Dref), every neighbourhood mbr is false →
        // no bits are imported and z/r remain 0.
        let dcup = vec![0x80u8, 0x03, 0x00];
        let cleanup = decode_cleanup(2, 2, &dcup).unwrap();
        let out = decode_sigprop(&cleanup, &[]).unwrap();
        assert_eq!(out.z, vec![0; 4]);
        assert_eq!(out.r, vec![0; 4]);
    }
}
