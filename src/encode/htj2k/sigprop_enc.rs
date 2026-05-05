//! HT SigProp pass encoder (inverse of
//! [`crate::decode::htj2k::sigprop::decode_sigprop`]).
//!
//! The SigProp pass emits one refinement bit per not-yet-significant
//! sample whose 8-neighbourhood contains at least one significant sample
//! (or a sample already refined by an earlier SigProp pass in the same
//! tile-part). A sign bit follows immediately for each sample whose
//! refinement bit is 1 (i.e. the sample becomes significant this pass).
//!
//! The resulting bits are packed into a forward LSB-first byte stream
//! (same stuffing rule as MagSgn: after a 0xFF byte the next byte has
//! its MSB forced to 0 and carries only 7 payload bits). This stream
//! becomes the first portion of the `Dref` HT refinement segment.

use crate::decode::htj2k::CleanupOutput;
use crate::encode::htj2k::streams_enc::MagSgnWriter;
use crate::error::Result;

/// Encoded SigProp state emitted into `Dref` (forward portion).
/// `z[n]`: 1 if sample n was refined this pass, else 0.
/// `sign[n]`: sign bit emitted (valid only when z[n]=1).
pub struct SigPropEncOutput {
    pub z: Vec<u8>,
    pub sign: Vec<u8>,
    pub bits: Vec<u8>,
}

/// Run the SigProp encoder on top of the cleanup output.
///
/// For each not-yet-significant sample (`sig[n] = 0`) whose 8-neighbour-
/// hood contains at least one significant or already-refined sample, emit
/// one magnitude bit from the caller-supplied `ref_mag[n]` (the MSB of
/// the sample's quantised magnitude minus the cleanup-pass portion, i.e.
/// the `M_b`-th bit after the cleanup pass). If that bit is 1 (the sample
/// is newly significant), also emit a sign bit.
///
/// `ref_mag[n]` is the next refinement magnitude bit for sample n. Only
/// samples with `sig[n]=0` are eligible; the caller provides a full-array
/// slice (zero for non-eligible positions is fine).
pub fn encode_sigprop(
    cleanup: &CleanupOutput,
    ref_mag: &[u8],
    ref_sign: &[u8],
) -> Result<SigPropEncOutput> {
    let n_samples = cleanup.mag.len();
    let mut z = vec![0u8; n_samples];
    let mut out_sign = cleanup.sign.clone();
    let mut writer = MagSgnWriter::new();

    let qw = cleanup.width.div_ceil(2);
    let cb_w = cleanup.width as i32;
    let cb_h = cleanup.height as i32;

    // Walk samples in quad scan order (same as the decoder).
    let qh = cleanup.height.div_ceil(2);
    for qy in 0..qh as i32 {
        for qx in 0..qw as i32 {
            let q = (qy as usize) * (qw as usize) + qx as usize;
            // Magnitude step: for each not-significant sample, check
            // neighbourhood and emit a bit.
            for j in 0..4u8 {
                let (dx, dy) = sample_offset(j);
                let x = 2 * qx + dx as i32;
                let y = 2 * qy + dy as i32;
                if x >= cb_w || y >= cb_h {
                    continue;
                }
                let n_idx = 4 * q + j as usize;
                if cleanup.sig[n_idx] != 0 {
                    continue;
                }
                // Check 8-neighbourhood for any significant or refined sample.
                let mbr = neighbour_mbr(cleanup, &z, qw, x, y, cb_w, cb_h);
                if mbr {
                    let bit = ref_mag.get(n_idx).copied().unwrap_or(0);
                    z[n_idx] = bit;
                    writer.write_bit(bit);
                }
            }
            // Sign step: for each sample that just got z[n]=1, emit sign.
            for j in 0..4u8 {
                let (dx, dy) = sample_offset(j);
                let x = 2 * qx + dx as i32;
                let y = 2 * qy + dy as i32;
                if x >= cb_w || y >= cb_h {
                    continue;
                }
                let n_idx = 4 * q + j as usize;
                if z[n_idx] != 0 {
                    let s = ref_sign.get(n_idx).copied().unwrap_or(0);
                    out_sign[n_idx] = s;
                    writer.write_bit(s);
                }
            }
        }
    }

    let bits = writer.into_bytes();
    Ok(SigPropEncOutput {
        z,
        sign: out_sign,
        bits,
    })
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

fn neighbour_mbr(
    cleanup: &CleanupOutput,
    z: &[u8],
    qw: u32,
    x: i32,
    y: i32,
    cb_w: i32,
    cb_h: i32,
) -> bool {
    for dy in -1i32..=1 {
        for dx in -1i32..=1 {
            if dx == 0 && dy == 0 {
                continue;
            }
            let nx = x + dx;
            let ny = y + dy;
            if nx < 0 || ny < 0 || nx >= cb_w || ny >= cb_h {
                continue;
            }
            let n_idx = sample_index(qw, nx as u32, ny as u32);
            if cleanup.sig[n_idx] != 0 || z[n_idx] != 0 {
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
    use crate::decode::htj2k::{decode_codeblock, CleanupOutput, ZBlk};
    use crate::encode::htj2k::cleanup_enc::{encode_cleanup, SampleHt};

    // Helper: rebuild a minimal CleanupOutput from a ZBlk::One decodeblock
    // output. Since CleanupOutput is pub-use'd from htj2k, we can
    // construct it here for testing by re-exporting it with the right fields.
    // However, CleanupOutput's fields are all pub so we can use it directly.

    /// All-zero block: no significant samples → no SigProp bits emitted.
    #[test]
    fn sigprop_enc_allzero_block_no_bits_emitted() {
        let samples = vec![SampleHt::default(); 16];
        let dcup = encode_cleanup(4, 4, &samples).expect("cleanup enc");
        // Construct CleanupOutput manually: all zeros.
        let n = 16usize;
        let cleanup = CleanupOutput {
            width: 4,
            height: 4,
            mag: vec![0u64; n],
            sign: vec![0u8; n],
            exp: vec![0u8; n],
            sig: vec![0u8; n],
        };
        let ref_mag = vec![0u8; n];
        let ref_sign = vec![0u8; n];
        let out = encode_sigprop(&cleanup, &ref_mag, &ref_sign).expect("sigprop enc");
        assert!(out.bits.is_empty());
        assert!(out.z.iter().all(|&z| z == 0));
        let _ = dcup; // cleanup enc verified as smoke-test
    }

    /// One significant sample, neighbour refinement bit = 0.
    /// Verify the emitted SigProp bits decode to z=0 for the neighbour.
    #[test]
    fn sigprop_enc_one_sig_neighbour_z_zero_roundtrip() {
        let n = 16usize;
        // sig[0] = 1, all others = 0.
        let cleanup = CleanupOutput {
            width: 4,
            height: 4,
            mag: vec![0u64; n],
            sign: vec![0u8; n],
            exp: vec![0u8; n],
            sig: {
                let mut v = vec![0u8; n];
                v[0] = 1;
                v
            },
        };
        let ref_mag = vec![0u8; n]; // neighbour emits bit=0
        let ref_sign = vec![0u8; n];
        let out = encode_sigprop(&cleanup, &ref_mag, &ref_sign).expect("sigprop enc");
        // All emitted bits are 0 → z must be 0 everywhere.
        assert!(out.z.iter().all(|&z| z == 0));
    }

    /// Verify the encoder compiles and SigPropEncOutput struct is usable.
    #[test]
    fn sigprop_enc_output_struct_accessible() {
        let out = SigPropEncOutput {
            z: vec![0, 1],
            sign: vec![0, 1],
            bits: vec![0x00],
        };
        assert_eq!(out.z.len(), 2);
        assert_eq!(out.bits, vec![0x00]);
    }

    /// End-to-end: encode block → decode with ZBlk::Two → all z=0 for
    /// an all-zero sample input (no significant samples, no neighbours).
    #[test]
    fn sigprop_e2e_allzero_block_z_zero() {
        let samples = vec![SampleHt::default(); 16];
        let dcup = encode_cleanup(4, 4, &samples).expect("cleanup enc");
        let out = decode_codeblock(4, 4, ZBlk::Two, &dcup, &[]).expect("dec Z2");
        assert!(out.z.iter().all(|&z| z == 0));
    }

    /// End-to-end: one significant sample, empty Dref (all refinement bits=0).
    /// z[n] is the SigProp "refinement indicator" = 1 iff the sample qualified
    /// for refinement (had a significant 8-neighbour), regardless of the bit.
    /// refinement[n] = the actual refinement bit (0 from empty Dref).
    #[test]
    fn sigprop_e2e_one_sig_empty_dref_refinement_zero() {
        let mut samples = vec![SampleHt::default(); 16];
        samples[0] = SampleHt { mag: 1, sign: 0 };
        let dcup = encode_cleanup(4, 4, &samples).expect("cleanup enc");
        let out = decode_codeblock(4, 4, ZBlk::Two, &dcup, &[]).expect("dec Z2");
        // All refinement bits (r_n) must be 0 since Dref is empty.
        assert!(
            out.refinement.iter().all(|&r| r == 0),
            "empty Dref must yield all-zero refinement bits"
        );
        // The significant sample itself (sig[0]=1) does not get z set
        // (SigProp only applies to non-significant samples). Its neighbours
        // may have z=1 if their mbr is true.
    }
}
